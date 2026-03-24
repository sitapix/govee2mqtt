use crate::ble::{Base64HexBytes, SetHumidifierMode, SetHumidifierNightlightParams};
use crate::lan_api::{Client as LanClient, DeviceStatus as LanDeviceStatus, LanDevice};
use crate::platform_api::{DeviceCapability, DeviceType, GoveeApiClient, HttpRequestFailed};
use crate::service::coordinator::Coordinator;
use crate::service::device::Device;
use crate::service::hass::{topic_safe_id, topic_safe_string, HassClient};
use crate::service::iot::IotClient;
use crate::temperature::{TemperatureScale, TemperatureValue};
use crate::undoc_api::{GoveeUndocumentedApi, LightEffectCategory};
use anyhow::Context;
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard, Semaphore};
use tokio::time::{sleep, Duration};

pub struct State {
    devices_by_id: Mutex<HashMap<String, Device>>,
    semaphore_by_id: Mutex<HashMap<String, Arc<Semaphore>>>,
    lan_client: Mutex<Option<LanClient>>,
    platform_client: Mutex<Option<GoveeApiClient>>,
    undoc_client: Mutex<Option<GoveeUndocumentedApi>>,
    iot_client: Mutex<Option<IotClient>>,
    hass_client: Mutex<Option<HassClient>>,
    hass_discovery_prefix: Mutex<String>,
    temperature_scale: Mutex<TemperatureScale>,
    pub event_bus: crate::service::event_bus::EventBus,
    /// Govee official MQTT push stats
    pub push_connected: std::sync::atomic::AtomicBool,
    pub push_event_count: std::sync::atomic::AtomicU64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            devices_by_id: Default::default(),
            semaphore_by_id: Default::default(),
            lan_client: Default::default(),
            platform_client: Default::default(),
            undoc_client: Default::default(),
            iot_client: Default::default(),
            hass_client: Default::default(),
            hass_discovery_prefix: Default::default(),
            temperature_scale: Default::default(),
            event_bus: crate::service::event_bus::EventBus::new(),
            push_connected: std::sync::atomic::AtomicBool::new(false),
            push_event_count: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

pub type StateHandle = Arc<State>;

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set_temperature_scale(&self, scale: TemperatureScale) {
        *self.temperature_scale.lock().await = scale;
    }

    pub async fn get_temperature_scale(&self) -> TemperatureScale {
        *self.temperature_scale.lock().await
    }

    pub async fn set_hass_disco_prefix(&self, prefix: String) {
        *self.hass_discovery_prefix.lock().await = prefix;
    }

    pub async fn get_hass_disco_prefix(&self) -> String {
        self.hass_discovery_prefix.lock().await.to_string()
    }

    /// Returns a mutable version of the specified device, creating
    /// an entry for it if necessary.
    pub async fn device_mut(&self, sku: &str, id: &str) -> MappedMutexGuard<'_, Device> {
        let devices = self.devices_by_id.lock().await;
        MutexGuard::map(devices, |devices| {
            devices
                .entry(id.to_string())
                .or_insert_with(|| Device::new(sku, id))
        })
    }

    pub async fn devices(&self) -> Vec<Device> {
        self.devices_by_id.lock().await.values().cloned().collect()
    }

    /// Returns an immutable copy of the specified Device
    pub async fn device_by_id(&self, id: &str) -> Option<Device> {
        let devices = self.devices_by_id.lock().await;
        devices.get(id).cloned()
    }

    async fn semaphore_for_device(&self, device: &Device) -> Arc<Semaphore> {
        self.semaphore_by_id
            .lock()
            .await
            .entry(device.id.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    pub async fn resolve_device_read_only(self: &Arc<Self>, label: &str) -> anyhow::Result<Device> {
        self.resolve_device(label)
            .await
            .ok_or_else(|| anyhow::anyhow!("device '{label}' not found"))
    }

    /// Resolve a device based on its label.
    /// Assuming the device is found, returns a Coordinator, which is a
    /// struct that ensures that only one task at a time can be processing
    /// control requests for a device.
    /// This method will not return until the calling task is permitted
    /// to proceed with its control attempt.
    pub async fn resolve_device_for_control(
        self: &Arc<Self>,
        label: &str,
    ) -> anyhow::Result<Coordinator> {
        let device = self
            .resolve_device(label)
            .await
            .ok_or_else(|| anyhow::anyhow!("device '{label}' not found"))?;
        let semaphore = self.semaphore_for_device(&device).await;
        let permit = semaphore.acquire_owned().await?;
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Schedule a task that will poll the device a short
        // time after the Coordinator is dropped, to reconcile
        // any changed state
        let state = self.clone();
        let device_id = device.id.to_string();
        tokio::spawn(async move {
            let _ = rx.await;
            state.poll_after_control(device_id).await
        });

        Ok(Coordinator::new(device, permit, tx))
    }

    /// Resolve a device using its name, computed name, id or label,
    /// ignoring case.
    pub async fn resolve_device(&self, label: &str) -> Option<Device> {
        let devices = self.devices_by_id.lock().await;

        // Try by id first
        if let Some(device) = devices.get(label) {
            return Some(device.clone());
        }

        for d in devices.values() {
            if d.name().eq_ignore_ascii_case(label)
                || d.id.eq_ignore_ascii_case(label)
                || topic_safe_id(d).eq_ignore_ascii_case(label)
                || topic_safe_string(&d.id).eq_ignore_ascii_case(label)
                || d.ip_addr()
                    .map(|ip| ip.to_string().eq_ignore_ascii_case(label))
                    .unwrap_or(false)
                || d.computed_name().eq_ignore_ascii_case(label)
            {
                return Some(d.clone());
            }
        }

        None
    }

    pub async fn set_hass_client(&self, client: HassClient) {
        self.hass_client.lock().await.replace(client);
    }

    pub async fn get_hass_client(&self) -> Option<HassClient> {
        self.hass_client.lock().await.clone()
    }

    pub async fn set_iot_client(&self, client: IotClient) {
        self.iot_client.lock().await.replace(client);
    }

    pub async fn get_iot_client(&self) -> Option<IotClient> {
        self.iot_client.lock().await.clone()
    }

    pub async fn set_lan_client(&self, client: LanClient) {
        self.lan_client.lock().await.replace(client);
    }

    pub async fn get_lan_client(&self) -> Option<LanClient> {
        self.lan_client.lock().await.clone()
    }

    pub async fn set_platform_client(&self, client: GoveeApiClient) {
        self.platform_client.lock().await.replace(client);
    }

    pub async fn get_platform_client(&self) -> Option<GoveeApiClient> {
        self.platform_client.lock().await.clone()
    }

    pub async fn set_undoc_client(&self, client: GoveeUndocumentedApi) {
        self.undoc_client.lock().await.replace(client);
    }

    pub async fn get_undoc_client(&self) -> Option<GoveeUndocumentedApi> {
        self.undoc_client.lock().await.clone()
    }

    /// Returns the IoT client and device's undoc entry if IoT control
    /// is available for the given device.
    async fn iot_for_device<'d>(
        &self,
        device: &'d Device,
    ) -> Option<(IotClient, &'d crate::undoc_api::DeviceEntry)> {
        if !device.iot_api_supported() {
            return None;
        }
        let iot = self.get_iot_client().await?;
        let entry = match device.undoc_device_info.as_ref() {
            Some(info) => &info.entry,
            None => {
                log::trace!(
                    "device {device} reports IoT supported but has no undoc entry; \
                     falling back to Platform API"
                );
                return None;
            }
        };
        Some((iot, entry))
    }

    pub async fn poll_iot_api(self: &Arc<Self>, device: &Device) -> anyhow::Result<bool> {
        if let Some(iot) = self.get_iot_client().await {
            if let Some(info) = device.undoc_device_info.clone() {
                if iot.is_device_compatible(&info.entry) {
                    let device_state = device.device_state();
                    log::trace!("requesting update via IoT MQTT {device} {device_state:?}");
                    match iot
                        .request_status_update(&info.entry)
                        .await
                        .context("iot.request_status_update")
                    {
                        Err(err) => {
                            log::error!("IoT status request failed for {device}: {err:#}");
                        }
                        Ok(()) => {
                            // The response will come in async via the mqtt loop in iot.rs
                            // However, if the device is offline, nothing will change our state.
                            // Let's explicitly mark the device as having been polled so that
                            // we don't keep sending a request every minute.
                            self.device_mut(&device.sku, &device.id)
                                .await
                                .set_last_polled();

                            return Ok(true);
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    pub async fn poll_platform_api(self: &Arc<Self>, device: &Device) -> anyhow::Result<bool> {
        // `device` is a cloned snapshot from state.devices(). The cooldown field
        // may be stale within a single poll cycle, but is accurate across cycles
        // since the 30-min cooldown far exceeds the 30s poll interval.
        if let Some(until) = device.platform_not_belong_until {
            if chrono::Utc::now() < until {
                log::trace!(
                    "device {device} skipped: 'devices not belong you' cooldown until {until}"
                );
                return Ok(false);
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let DeviceType::Other(other) = &device.device_type() {
                // Cannot poll an unknown device
                // <https://github.com/wez/govee2mqtt/issues/391>
                // <https://github.com/wez/govee2mqtt/issues/501>
                // <https://github.com/wez/govee2mqtt/issues/394>
                log::trace!("device {device} cannot be polled because it has type Other: {other}");
                return Ok(false);
            }

            let device_state = device.device_state();
            log::trace!("requesting update via Platform API {device} {device_state:?}");
            if let Some(info) = &device.http_device_info {
                match client.get_device_state(info).await {
                    Ok(http_state) => {
                        log::trace!("updated state for {device}");
                        {
                            let mut device = self.device_mut(&device.sku, &device.id).await;
                            device.set_http_device_state(http_state);
                            device.set_last_polled();
                        }
                        self.notify_of_state_change(&device.id)
                            .await
                            .context("state.notify_of_state_change")?;
                        return Ok(true);
                    }
                    Err(err) => {
                        // Govee returns HTTP 200 with embedded status 400 and
                        // msg "devices not belong you" for devices no longer
                        // associated with the account (or BLE-only devices).
                        if let Some(http_err) = HttpRequestFailed::from_err(&err) {
                            if http_err.content_contains("devices not belong you") {
                                let retry_at = chrono::Utc::now() + chrono::Duration::minutes(30);
                                log::warn!(
                                    "Device {device} is not associated with your Govee account \
                                     (or is BLE-only). Skipping Platform API polls for 30 minutes. \
                                     If this device should be yours, re-add it in the Govee app."
                                );
                                self.device_mut(&device.sku, &device.id)
                                    .await
                                    .platform_not_belong_until = Some(retry_at);
                                return Ok(false);
                            }
                        }
                        return Err(err).context("get_device_state");
                    }
                }
            }
        } else {
            log::trace!(
                "device {device} wanted a status update, but there is no platform client available"
            );
        }
        Ok(false)
    }

    async fn poll_lan_api<F: Fn(&LanDeviceStatus) -> bool>(
        self: &Arc<Self>,
        device: &LanDevice,
        acceptor: F,
    ) -> anyhow::Result<()> {
        match self.get_lan_client().await {
            Some(client) => {
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() <= deadline {
                    let status = client.query_status(device).await?;
                    let accepted = (acceptor)(&status);
                    self.device_mut(&device.sku, &device.device)
                        .await
                        .set_lan_device_status(status);
                    if accepted {
                        break;
                    }
                    sleep(Duration::from_millis(100)).await;
                }
                self.notify_of_state_change(&device.device).await?;
                Ok(())
            }
            None => anyhow::bail!("no lan client"),
        }
    }

    pub async fn device_control<V: Into<JsonValue>>(
        self: &Arc<Self>,
        device: &Device,
        capability: &DeviceCapability,
        value: V,
    ) -> anyhow::Result<()> {
        let value: JsonValue = value.into();
        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to send {value:?} control to {device}");
                client.control_device(info, capability, value).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to use Platform API to control {device}");
    }

    pub async fn device_light_power_on(
        self: &Arc<Self>,
        device: &Device,
        on: bool,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| p.on = on)
            .await?
        {
            return Ok(());
        }

        let instance_name = device
            .get_light_power_toggle_instance_name()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Don't know how to toggle just the light portion of {device}. \
                     Please share the device metadata and state if you report this issue"
                )
            })?;

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} light power state");
            lan_dev.send_turn(on).await?;
            self.poll_lan_api(lan_dev, |status| status.on == on).await?;
            return Ok(());
        }

        if let Some((iot, entry)) = self.iot_for_device(device).await {
            log::info!("Using IoT API to set {device} light power state");
            iot.set_power_state(entry, on).await?;
            return Ok(());
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} light {instance_name} state");
                client.set_toggle_state(info, instance_name, on).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to control light power state for {device}");
    }

    pub async fn device_power_on(
        self: &Arc<Self>,
        device: &Device,
        on: bool,
    ) -> anyhow::Result<()> {
        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} power state");
            lan_dev.send_turn(on).await?;
            self.poll_lan_api(lan_dev, |status| status.on == on).await?;
            return Ok(());
        }

        if let Some((iot, entry)) = self.iot_for_device(device).await {
            log::info!("Using IoT API to set {device} power state");
            iot.set_power_state(entry, on).await?;
            return Ok(());
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} power state");
                client.set_power_state(info, on).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to control power state for {device}");
    }

    pub async fn device_set_brightness(
        self: &Arc<Self>,
        device: &Device,
        percent: u8,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| {
                p.brightness = percent;
                p.on = true;
            })
            .await?
        {
            return Ok(());
        }

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} brightness");
            lan_dev.send_brightness(percent).await?;
            self.poll_lan_api(lan_dev, |status| status.brightness == percent)
                .await?;
            return Ok(());
        }

        if let Some((iot, entry)) = self.iot_for_device(device).await {
            log::info!("Using IoT API to set {device} brightness");
            iot.set_brightness(entry, percent).await?;
            return Ok(());
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} brightness");
                client.set_brightness(info, percent).await?;
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control brightness for {device}");
    }

    pub async fn device_set_color_temperature(
        self: &Arc<Self>,
        device: &Device,
        kelvin: u32,
    ) -> anyhow::Result<()> {
        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} color temperature");
            lan_dev.send_color_temperature_kelvin(kelvin).await?;
            self.poll_lan_api(lan_dev, |status| status.color_temperature_kelvin == kelvin)
                .await?;
            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
            return Ok(());
        }

        if let Some((iot, entry)) = self.iot_for_device(device).await {
            log::info!("Using IoT API to set {device} color temperature");
            iot.set_color_temperature(entry, kelvin).await?;
            return Ok(());
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} color temperature");
                client.set_color_temperature(info, kelvin).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(None);
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control color temperature for {device}");
    }

    // FIXME: this function probably shouldn't exist here
    async fn try_humidifier_set_nightlight<F: Fn(&mut SetHumidifierNightlightParams)>(
        self: &Arc<Self>,
        device: &Device,
        apply: F,
    ) -> anyhow::Result<bool> {
        let mut params: SetHumidifierNightlightParams =
            device.nightlight_state.clone().unwrap_or_default().into();
        (apply)(&mut params);

        if let Ok(command) = Base64HexBytes::encode_for_sku(&device.sku, &params) {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} color");
                    iot.send_real(&info.entry, command.base64()).await?;
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    pub async fn humidifier_set_parameter(
        self: &Arc<Self>,
        device: &Device,
        work_mode: i64,
        value: i64,
    ) -> anyhow::Result<()> {
        if let Ok(command) = Base64HexBytes::encode_for_sku(
            &device.sku,
            &SetHumidifierMode {
                mode: work_mode as u8,
                param: value as u8,
            },
        ) {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    iot.send_real(&info.entry, command.base64()).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                client.set_work_mode(info, work_mode, value).await?;
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control humidifier parameter work_mode={work_mode} for {device}");
    }

    pub async fn device_set_color_rgb(
        self: &Arc<Self>,
        device: &Device,
        r: u8,
        g: u8,
        b: u8,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| {
                p.r = r;
                p.g = g;
                p.b = b;
                p.on = true;
            })
            .await?
        {
            return Ok(());
        }

        if let Some(lan_dev) = &device.lan_device {
            let color = crate::lan_api::DeviceColor { r, g, b };
            log::info!("Using LAN API to set {device} color");
            lan_dev.send_color_rgb(color).await?;
            self.poll_lan_api(lan_dev, |status| status.color == color)
                .await?;
            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
            return Ok(());
        }

        if let Some((iot, entry)) = self.iot_for_device(device).await {
            log::info!("Using IoT API to set {device} color");
            iot.set_color_rgb(entry, r, g, b).await?;
            return Ok(());
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} color");
                client.set_color_rgb(info, r, g, b).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(None);
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control color for {device}");
    }

    pub async fn poll_after_control(self: &Arc<Self>, id: String) {
        let Some(device) = self.device_by_id(&id).await else {
            return;
        };

        let iot_available = self.get_iot_client().await.is_some();

        if device.pollable_via_iot() && iot_available {
            return;
        }
        if device.pollable_via_lan() {
            return;
        }

        // Add a slight delay, as the status returned
        // by the platform API isn't guaranteed to be
        // coherent with the command we just issued
        // right away :-/
        sleep(Duration::from_secs(5)).await;

        log::info!("Polling {device} to get latest state after control");
        if let Err(err) = self.poll_platform_api(&device).await {
            log::error!("Polling {device} failed: {err:#}");
        }
    }

    pub async fn device_list_scenes(&self, device: &Device) -> anyhow::Result<Vec<String>> {
        // TODO: some plumbing to maintain offline scene controls for preferred-LAN control
        let mut platform_scenes = vec![];
        let mut undoc_scenes = vec![];

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                match client.list_scene_names(info).await {
                    Ok(names) => platform_scenes.extend(names),
                    Err(err) => {
                        log::warn!("Unable to list Platform API scenes for {device}: {err:#}");
                    }
                }
            }
        }

        match GoveeUndocumentedApi::get_scenes_for_device(&device.sku).await {
            Ok(categories) => undoc_scenes.extend(scene_names_from_undoc_categories(categories)),
            Err(err) => {
                log::trace!("Undocumented scene catalog unavailable for {device}: {err:#}");
            }
        }

        let mut scenes = merge_scene_name_sources(platform_scenes, undoc_scenes);

        // Merge decoded scene database (AlgoClaw) as additional source
        let decoded_names =
            crate::service::scene_database::scene_names_for_sku(&device.sku);
        if !decoded_names.is_empty() {
            let existing_lower: std::collections::HashSet<String> =
                scenes.iter().map(|s| s.to_ascii_lowercase()).collect();
            for name in decoded_names {
                if !existing_lower.contains(&name.to_ascii_lowercase()) {
                    scenes.push(name);
                }
            }
            scenes = sort_and_dedup_scenes(scenes);
        }

        if scenes.is_empty() {
            log::trace!("No scene data available for {device} from any source");
        }

        Ok(scenes)
    }

    pub async fn device_list_capability_options(
        &self,
        device: &Device,
        instance: &str,
    ) -> anyhow::Result<Vec<String>> {
        if instance.eq_ignore_ascii_case("lightScene") {
            let options = self
                .device_list_scenes(device)
                .await?
                .into_iter()
                .filter(|scene| !scene.is_empty() && !scene.starts_with("Music: "))
                .collect::<Vec<_>>();

            if !options.is_empty() {
                return Ok(options);
            }
        }

        if let Some(info) = &device.http_device_info {
            if let Some(client) = self.get_platform_client().await {
                return Ok(sort_and_dedup_scenes(
                    client.list_capability_names(info, instance).await?,
                ));
            }
        }

        let options = enum_capability_names_from_device_info(device, instance);
        if !options.is_empty() {
            return Ok(sort_and_dedup_scenes(options));
        }

        Ok(vec![])
    }

    pub async fn device_list_music_modes(&self, device: &Device) -> anyhow::Result<Vec<String>> {
        if let Some(info) = &device.http_device_info {
            if let Some(client) = self.get_platform_client().await {
                return Ok(sort_and_dedup_scenes(client.list_music_mode_names(info)?));
            }

            let options = info
                .capability_by_instance("musicMode")
                .and_then(|cap| cap.struct_field_by_name("musicMode"))
                .and_then(|field| match &field.field_type {
                    crate::platform_api::DeviceParameters::Enum { options } => {
                        Some(options.iter().map(|opt| opt.name.to_string()).collect())
                    }
                    _ => None,
                })
                .unwrap_or_default();

            return Ok(sort_and_dedup_scenes(options));
        }

        Ok(vec![])
    }

    pub async fn device_set_target_temperature(
        self: &Arc<Self>,
        device: &Device,
        instance_name: &str,
        target: TemperatureValue,
    ) -> anyhow::Result<()> {
        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} target temperature to {target}");
                client
                    .set_target_temperature(info, instance_name, target)
                    .await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to set temperature for {device}");
    }

    pub async fn device_set_scene(
        self: &Arc<Self>,
        device: &Device,
        scene: &str,
    ) -> anyhow::Result<()> {
        // TODO: some plumbing to maintain offline scene controls for preferred-LAN control
        let avoid_platform_api = device.avoid_platform_api();

        if !avoid_platform_api {
            if let Some(client) = self.get_platform_client().await {
                if let Some(info) = &device.http_device_info {
                    log::info!("Using Platform API to set {device} to scene {scene}");
                    client.set_scene_by_name(info, scene).await?;
                    let mut device = self.device_mut(&device.sku, &device.id).await;
                    if let Some(mode) = scene.strip_prefix("Music: ") {
                        device.set_active_music_mode(mode, 100, true);
                    } else {
                        device.set_active_scene(Some(scene));
                    }
                    return Ok(());
                }
            }
        }

        if let Some(lan_dev) = &device.lan_device {
            // Try undocumented scene catalog first
            if let Ok(()) = lan_dev.set_scene_by_name(scene).await {
                log::info!("Using LAN API (undoc catalog) to set {device} to scene {scene}");
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(Some(scene));
                return Ok(());
            }

            // Fall back to decoded scene database (AlgoClaw ptReal commands)
            if let Some(commands) =
                crate::service::scene_database::scene_commands(&device.sku, scene)
            {
                log::info!(
                    "Using LAN API (decoded database) to set {device} to scene {scene} ({} commands)",
                    commands.len()
                );
                lan_dev.send_real(commands).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(Some(scene));
                return Ok(());
            }
        }

        // Also try decoded database via IoT if no LAN
        if let Some(commands) =
            crate::service::scene_database::scene_commands(&device.sku, scene)
        {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!(
                        "Using IoT API (decoded database) to set {device} to scene {scene}"
                    );
                    iot.send_real(&info.entry, commands).await?;
                    self.device_mut(&device.sku, &device.id)
                        .await
                        .set_active_scene(Some(scene));
                    return Ok(());
                }
            }
        }

        anyhow::bail!("Unable to set scene for {device}");
    }

    pub async fn device_set_capability_option(
        self: &Arc<Self>,
        device: &Device,
        instance: &str,
        option: &str,
    ) -> anyhow::Result<()> {
        let avoid_platform_api = device.avoid_platform_api();

        if !avoid_platform_api {
            if let Some(client) = self.get_platform_client().await {
                if let Some(info) = &device.http_device_info {
                    log::info!("Using Platform API to set {device} {instance} to {option}");
                    client
                        .set_capability_by_name(info, instance, option)
                        .await?;
                    self.device_mut(&device.sku, &device.id)
                        .await
                        .set_active_scene_for_instance(Some(instance), Some(option));
                    return Ok(());
                }
            }
        }

        // LAN fallback for scene-type capabilities
        if instance == "lightScene" {
            if let Some(lan_dev) = &device.lan_device {
                log::info!("Using LAN API to set {device} {instance} to {option}");
                lan_dev.set_scene_by_name(option).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene_for_instance(Some(instance), Some(option));
                return Ok(());
            }
        }

        anyhow::bail!("Unable to set {instance} for {device}");
    }

    pub async fn device_set_music_mode(
        self: &Arc<Self>,
        device: &Device,
        mode: &str,
    ) -> anyhow::Result<()> {
        let (sensitivity, auto_color) = device
            .active_music_mode()
            .map(|music| (music.sensitivity, music.auto_color))
            .unwrap_or((100, true));

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} music mode to {mode}");
                client
                    .set_music_mode(info, mode, sensitivity, auto_color)
                    .await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_music_mode(mode, sensitivity, auto_color);
                return Ok(());
            }
        }

        anyhow::bail!("Unable to set music mode for {device}");
    }

    pub async fn device_set_music_sensitivity(
        self: &Arc<Self>,
        device: &Device,
        sensitivity: u32,
    ) -> anyhow::Result<()> {
        let music = device
            .active_music_mode()
            .cloned()
            .or_else(|| {
                device.active_scene_name().and_then(|scene| {
                    scene.strip_prefix("Music: ").map(|mode| {
                        crate::service::device::ActiveMusicModeInfo {
                            mode: mode.to_string(),
                            sensitivity: 100,
                            auto_color: true,
                        }
                    })
                })
            })
            .ok_or_else(|| anyhow::anyhow!("Music mode is not currently active for {device}"))?;

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} music sensitivity to {sensitivity}");
                client
                    .set_music_mode(info, &music.mode, sensitivity, music.auto_color)
                    .await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .update_active_music_mode(Some(sensitivity), None)?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to set music sensitivity for {device}");
    }

    pub async fn device_set_music_auto_color(
        self: &Arc<Self>,
        device: &Device,
        auto_color: bool,
    ) -> anyhow::Result<()> {
        let music = device
            .active_music_mode()
            .cloned()
            .or_else(|| {
                device.active_scene_name().and_then(|scene| {
                    scene.strip_prefix("Music: ").map(|mode| {
                        crate::service::device::ActiveMusicModeInfo {
                            mode: mode.to_string(),
                            sensitivity: 100,
                            auto_color: true,
                        }
                    })
                })
            })
            .ok_or_else(|| anyhow::anyhow!("Music mode is not currently active for {device}"))?;

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} music autoColor to {auto_color}");
                client
                    .set_music_mode(info, &music.mode, music.sensitivity, auto_color)
                    .await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .update_active_music_mode(None, Some(auto_color))?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to set music autoColor for {device}");
    }

    // Take care not to call this while you hold a mutable device
    // reference, as that will deadlock!
    pub async fn notify_of_state_change(self: &Arc<Self>, device_id: &str) -> anyhow::Result<()> {
        let Some(canonical_device) = self.device_by_id(&device_id).await else {
            anyhow::bail!("cannot find device {device_id}!?");
        };

        // Emit event for any interested extensions
        self.event_bus.emit(crate::service::event_bus::Event::DeviceStateChanged {
            device_id: device_id.to_string(),
        });

        if let Some(hass) = self.get_hass_client().await {
            // Mark device as online since we just got a state update
            let avail_topic =
                crate::service::hass::device_availability_topic(&canonical_device);
            if let Err(err) = hass.publish_retained(&avail_topic, "online").await {
                log::warn!("Failed to publish device availability for {device_id}: {err:#}");
            }

            hass.advise_hass_of_light_state(&canonical_device, self)
                .await?;
        }

        Ok(())
    }

    /// Publish bridge-level "offline" status and disconnect cleanly.
    /// Per-device availability is handled by AvailabilityExtension::stop().
    pub async fn graceful_shutdown(self: &Arc<Self>) {
        log::info!("Graceful shutdown: publishing offline status");

        if let Some(hass) = self.get_hass_client().await {
            // Mark global bridge as offline
            if let Err(err) = hass
                .publish_retained(crate::service::hass::availability_topic(), "offline")
                .await
            {
                log::warn!("Failed to publish bridge offline: {err:#}");
            }

            // Publish bridge info with offline state
            let bridge_info = serde_json::json!({
                "version": crate::version_info::govee_version(),
                "state": "offline",
            });
            let _ = hass
                .publish_retained("gv2mqtt/bridge/info", bridge_info.to_string())
                .await;

            // Give MQTT time to flush
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        log::info!("Graceful shutdown complete");
    }

}

pub fn sort_and_dedup_scenes(scenes: Vec<String>) -> Vec<String> {
    let mut deduped = vec![];
    let mut seen = HashSet::new();
    let mut has_empty = false;

    for scene in scenes {
        if scene.is_empty() {
            has_empty = true;
            continue;
        }

        if seen.insert(scene.to_ascii_lowercase()) {
            deduped.push(scene);
        }
    }

    if has_empty {
        deduped.insert(0, String::new());
    }

    deduped
}

fn enum_capability_names_from_device_info(device: &Device, instance: &str) -> Vec<String> {
    let Some(info) = &device.http_device_info else {
        return vec![];
    };

    let Some(cap) = info.capability_by_instance(instance) else {
        return vec![];
    };

    let Some(crate::platform_api::DeviceParameters::Enum { options }) = &cap.parameters else {
        return vec![];
    };

    options.iter().map(|opt| opt.name.to_string()).collect()
}

fn merge_scene_name_sources(
    platform_scenes: Vec<String>,
    undoc_scenes: Vec<String>,
) -> Vec<String> {
    if undoc_scenes.is_empty() {
        return sort_and_dedup_scenes(platform_scenes);
    }

    let mut merged = undoc_scenes;
    merged.extend(platform_scenes);
    sort_and_dedup_scenes(merged)
}

fn scene_names_from_undoc_categories(categories: Vec<LightEffectCategory>) -> Vec<String> {
    let mut names = vec![];
    for cat in categories {
        for scene in cat.scenes {
            for effect in scene.light_effects {
                if effect.scene_code != 0 {
                    names.push(scene.scene_name);
                    break;
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::{
        merge_scene_name_sources, scene_names_from_undoc_categories, sort_and_dedup_scenes, State,
    };
    use crate::lan_api::LanDevice;
    use crate::platform_api::HttpDeviceInfo;
    use crate::undoc_api::{LightEffectCategory, LightEffectEntry, LightEffectScene};
    use serde_json::json;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    #[tokio::test]
    async fn resolve_device_matches_supported_labels_case_insensitively() {
        let state = Arc::new(State::new());
        let id = "AA:BB:CC:DD:EE:FF:42:2A";
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));

        {
            let mut device = state.device_mut("H6000", id).await;
            device.set_lan_device(LanDevice {
                ip,
                device: id.to_string(),
                sku: "H6000".to_string(),
                ble_version_hard: String::new(),
                ble_version_soft: String::new(),
                wifi_version_hard: String::new(),
                wifi_version_soft: String::new(),
            });
            device.http_device_info = Some(HttpDeviceInfo {
                sku: "H6000".to_string(),
                device: id.to_string(),
                device_name: "Desk Lamp".to_string(),
                device_type: Default::default(),
                capabilities: vec![],
            });
        }

        for label in [
            "Desk Lamp",
            "desk lamp",
            id,
            "aabbccddeeff422a",
            "aa_bb_cc_dd_ee_ff_42_2a",
            "H6000_422A",
            "192.168.1.50",
        ] {
            let resolved = state.resolve_device(label).await;
            assert_eq!(resolved.as_ref().map(|device| device.id.as_str()), Some(id));
        }
    }

    #[tokio::test]
    async fn resolve_device_read_only_returns_error_for_missing_device() {
        let state = Arc::new(State::new());
        let error = state
            .resolve_device_read_only("missing-device")
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("device 'missing-device' not found"));
    }

    #[test]
    fn scene_names_from_undoc_categories_only_includes_runnable_scenes() {
        let categories = vec![LightEffectCategory {
            category_id: 1,
            category_name: "Life".to_string(),
            scenes: vec![
                LightEffectScene {
                    scene_id: 10,
                    icon_urls: vec![],
                    scene_name: "Forest".to_string(),
                    analytic_name: "forest".to_string(),
                    scene_type: 0,
                    scene_code: 0,
                    scence_category_id: 1,
                    pop_up_prompt: 0,
                    scenes_hint: String::new(),
                    rule: json!({}),
                    light_effects: vec![LightEffectEntry {
                        scence_param_id: 100,
                        scence_name: "Forest".to_string(),
                        scence_param: String::new(),
                        scene_code: 1,
                        special_effect: vec![],
                        cmd_version: None,
                        scene_type: 0,
                        diy_effect_code: vec![],
                        diy_effect_str: String::new(),
                        rules: vec![],
                        speed_info: json!({}),
                    }],
                    voice_url: String::new(),
                    create_time: 0,
                },
                LightEffectScene {
                    scene_id: 11,
                    icon_urls: vec![],
                    scene_name: "Broken".to_string(),
                    analytic_name: "broken".to_string(),
                    scene_type: 0,
                    scene_code: 0,
                    scence_category_id: 1,
                    pop_up_prompt: 0,
                    scenes_hint: String::new(),
                    rule: json!({}),
                    light_effects: vec![LightEffectEntry {
                        scence_param_id: 101,
                        scence_name: "Broken".to_string(),
                        scence_param: String::new(),
                        scene_code: 0,
                        special_effect: vec![],
                        cmd_version: None,
                        scene_type: 0,
                        diy_effect_code: vec![],
                        diy_effect_str: String::new(),
                        rules: vec![],
                        speed_info: json!({}),
                    }],
                    voice_url: String::new(),
                    create_time: 0,
                },
            ],
        }];

        assert_eq!(
            scene_names_from_undoc_categories(categories),
            vec!["Forest".to_string()]
        );
    }

    #[test]
    fn sort_and_dedup_scenes_preserves_first_seen_order() {
        let merged = sort_and_dedup_scenes(vec![
            "Sunrise".to_string(),
            "Forest".to_string(),
            "forest".to_string(),
            "DIY Holiday".to_string(),
        ]);

        assert_eq!(
            merged,
            vec![
                "Sunrise".to_string(),
                "Forest".to_string(),
                "DIY Holiday".to_string(),
            ]
        );
    }

    #[test]
    fn sort_and_dedup_scenes_keeps_empty_option_first() {
        let merged = sort_and_dedup_scenes(vec![
            "Aurora".to_string(),
            "".to_string(),
            "Forest".to_string(),
            "aurora".to_string(),
        ]);

        assert_eq!(
            merged,
            vec!["".to_string(), "Aurora".to_string(), "Forest".to_string(),]
        );
    }

    #[test]
    fn merge_scene_name_sources_prefers_undoc_order_and_appends_platform_only_entries() {
        let merged = merge_scene_name_sources(
            vec![
                "".to_string(),
                "Rainbow-B".to_string(),
                "Sunrise".to_string(),
                "Work".to_string(),
                "Music: Dynamic".to_string(),
            ],
            vec![
                "Sunrise".to_string(),
                "Aurora".to_string(),
                "Work".to_string(),
            ],
        );

        assert_eq!(
            merged,
            vec![
                "".to_string(),
                "Sunrise".to_string(),
                "Aurora".to_string(),
                "Work".to_string(),
                "Rainbow-B".to_string(),
                "Music: Dynamic".to_string(),
            ]
        );
    }
}
