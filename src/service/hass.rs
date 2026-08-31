use crate::hass_mqtt::climate::mqtt_set_temperature;
use crate::hass_mqtt::enumerator::{enumerate_all_entites, enumerate_entities_for_device};
use crate::hass_mqtt::humidifier::{mqtt_device_set_work_mode, mqtt_humidifier_set_target};
use crate::hass_mqtt::instance::EntityList;
use crate::hass_mqtt::number::{mqtt_number_command, mqtt_set_music_sensitivity};
use crate::hass_mqtt::select::{
    mqtt_set_capability_option, mqtt_set_mode_scene, mqtt_set_music_mode,
};
use crate::hass_mqtt::switch::mqtt_set_music_auto_color;
use crate::lan_api::DeviceColor;
use crate::opt_env_var;
use crate::platform_api::{from_json, DeviceType};
use crate::service::device::Device as ServiceDevice;
use crate::service::state::StateHandle;
use crate::temperature::TemperatureScale;
use anyhow::Context;
use async_channel::Receiver;
use mosquitto_rs::router::{MqttRouter, Params, Payload, State};
use mosquitto_rs::{Client, Event, QoS};
#[cfg(test)]
use parking_lot::Mutex as ParkingMutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc as StdArc;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

const HASS_REGISTER_DELAY: tokio::time::Duration = tokio::time::Duration::from_secs(15);
const DISCOVERY_TOPICS_CACHE_FILE: &str = "gv2mqtt-discovery-topics.json";

#[derive(clap::Parser, Debug)]
pub struct HassArguments {
    /// The mqtt broker hostname or address.
    /// You may also set this via the GOVEE_MQTT_HOST environment variable.
    #[arg(long, global = true)]
    mqtt_host: Option<String>,

    /// The mqtt broker port
    /// You may also set this via the GOVEE_MQTT_PORT environment variable.
    /// If unspecified, uses 1883
    #[arg(long, global = true)]
    mqtt_port: Option<u16>,

    /// The username to authenticate against the broker
    /// You may also set this via the GOVEE_MQTT_USER environment variable.
    #[arg(long, global = true)]
    mqtt_username: Option<String>,

    /// The password to authenticate against the broker
    /// You may also set this via the GOVEE_MQTT_PASSWORD environment variable.
    #[arg(long, global = true)]
    mqtt_password: Option<String>,

    #[arg(long, global = true)]
    mqtt_bind_address: Option<String>,

    #[arg(long, global = true, default_value = "homeassistant")]
    hass_discovery_prefix: String,

    /// The temperature scale to use when showing temperature values as
    /// entities in home assistant. Can be either "C" or "F" for Celsius
    /// or Fahrenheit respectively.
    /// You may also set this via the GOVEE_TEMPERATURE_SCALE environment
    /// variable.
    #[arg(long, global = true)]
    temperature_scale: Option<String>,
}

impl HassArguments {
    pub fn opt_mqtt_host(&self) -> anyhow::Result<Option<String>> {
        match &self.mqtt_host {
            Some(h) => Ok(Some(h.to_string())),
            None => opt_env_var("GOVEE_MQTT_HOST"),
        }
    }

    pub fn mqtt_host(&self) -> anyhow::Result<String> {
        self.opt_mqtt_host()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Please specify the mqtt broker either via the \
                --mqtt-host parameter or by setting $GOVEE_MQTT_HOST"
            )
        })
    }

    pub fn mqtt_port(&self) -> anyhow::Result<u16> {
        match self.mqtt_port {
            Some(p) => Ok(p),
            None => Ok(opt_env_var("GOVEE_MQTT_PORT")?.unwrap_or(1883)),
        }
    }

    pub fn mqtt_username(&self) -> anyhow::Result<Option<String>> {
        match self.mqtt_username.clone() {
            Some(u) => Ok(Some(u)),
            None => opt_env_var("GOVEE_MQTT_USER"),
        }
    }

    pub fn mqtt_password(&self) -> anyhow::Result<Option<String>> {
        match self.mqtt_password.clone() {
            Some(u) => Ok(Some(u)),
            None => opt_env_var("GOVEE_MQTT_PASSWORD"),
        }
    }

    pub fn temperature_scale(&self) -> anyhow::Result<TemperatureScale> {
        match &self.temperature_scale {
            Some(s) => Ok(s.parse()?),
            None => {
                Ok(opt_env_var("GOVEE_TEMPERATURE_SCALE")?.unwrap_or(TemperatureScale::Celsius))
            }
        }
    }
}

#[derive(Clone)]
enum HassClientBackend {
    Mqtt(Client),
    #[cfg(test)]
    Capture(StdArc<ParkingMutex<Vec<(String, String)>>>),
}

#[derive(Clone)]
pub struct HassClient {
    backend: HassClientBackend,
    published_config_topics: Arc<StdMutex<BTreeSet<String>>>,
}

impl HassClient {
    pub fn from_client(client: Client) -> Self {
        Self {
            backend: HassClientBackend::Mqtt(client),
            published_config_topics: Arc::new(StdMutex::new(BTreeSet::new())),
        }
    }

    #[cfg(test)]
    pub fn new_test() -> Self {
        Self {
            backend: HassClientBackend::Capture(StdArc::new(ParkingMutex::new(vec![]))),
            published_config_topics: Arc::new(StdMutex::new(BTreeSet::new())),
        }
    }

    #[cfg(test)]
    pub fn published_messages(&self) -> Vec<(String, String)> {
        match &self.backend {
            HassClientBackend::Capture(messages) => messages.lock().clone(),
            HassClientBackend::Mqtt(_) => vec![],
        }
    }

    /// Re-register all entities with Home Assistant.
    /// Call after config changes to update names, icons, availability, etc.
    pub async fn re_register(&self, state: &StateHandle) -> anyhow::Result<()> {
        self.register_with_hass(state).await
    }

    async fn register_with_hass(&self, state: &StateHandle) -> anyhow::Result<()> {
        let entities = enumerate_all_entites(state).await?;
        self.clear_recorded_config_topics();

        // Register the configs
        log::trace!("register_with_hass: register entities");
        entities.publish_config(state, self).await?;
        self.purge_stale_discovery_topics(state).await?;

        // Allow hass extra time to register the entities before
        // we mark them as available
        let delay = tokio::time::Duration::from_millis((10 * entities.len()) as u64);
        log::info!(
            "Wait {delay:?} for hass to settle on {} entity configs",
            entities.len()
        );
        tokio::time::sleep(delay).await;

        // Mark as available
        log::trace!("register_with_hass: mark as online");
        self.publish_retained(availability_topic(), "online")
            .await
            .context("online -> availability_topic")?;

        // Publish bridge info
        let bridge_info = serde_json::json!({
            "version": crate::version_info::govee_version(),
            "state": "online",
        });
        self.publish_retained("gv2mqtt/bridge/info", bridge_info.to_string())
            .await
            .context("publish bridge info")?;

        // report initial state
        log::trace!("register_with_hass: reporting state");
        entities.notify_state(self).await.context("notify_state")?;

        log::trace!("register_with_hass: done");

        Ok(())
    }

    fn record_config_topic(&self, topic: &str) {
        if topic.ends_with("/config") {
            self.published_config_topics
                .lock()
                .expect("published_config_topics lock")
                .insert(topic.to_string());
        }
    }

    fn clear_recorded_config_topics(&self) {
        self.published_config_topics
            .lock()
            .expect("published_config_topics lock")
            .clear();
    }

    fn recorded_config_topics(&self) -> Vec<String> {
        self.published_config_topics
            .lock()
            .expect("published_config_topics lock")
            .iter()
            .cloned()
            .collect()
    }

    async fn purge_stale_discovery_topics(&self, state: &StateHandle) -> anyhow::Result<()> {
        let current_topics = self.recorded_config_topics();
        let prior_topics = load_discovery_topics_cache();
        let current_topic_set: BTreeSet<_> = current_topics.iter().cloned().collect();

        for topic in prior_topics {
            if current_topic_set.contains(&topic) {
                continue;
            }

            log::info!("Removing stale Home Assistant discovery topic {topic}");
            self.publish_retained(&topic, "").await?;
        }

        for topic in self
            .legacy_discovery_topics_to_clear(state, &current_topic_set)
            .await?
        {
            log::info!("Removing legacy Home Assistant discovery topic {topic}");
            self.publish_retained(&topic, "").await?;
        }

        save_discovery_topics_cache(&current_topics)?;
        Ok(())
    }

    async fn legacy_discovery_topics_to_clear(
        &self,
        state: &StateHandle,
        current_topic_set: &BTreeSet<String>,
    ) -> anyhow::Result<Vec<String>> {
        let mut topics = vec![];

        for device in state.devices().await {
            if device.device_type() != DeviceType::Light {
                continue;
            }

            let mut has_dedicated_scene_controls = false;
            for instance in ["lightScene", "diyScene", "snapshot", "nightlightScene"] {
                if !state
                    .device_list_capability_options(&device, instance)
                    .await?
                    .is_empty()
                {
                    has_dedicated_scene_controls = true;
                    break;
                }
            }

            if !has_dedicated_scene_controls
                && !state.device_list_music_modes(&device).await?.is_empty()
            {
                has_dedicated_scene_controls = true;
            }

            if !has_dedicated_scene_controls {
                continue;
            }

            let legacy_topic = format!(
                "{}/select/gv2mqtt-{}-mode-scene/config",
                state.get_hass_disco_prefix().await,
                topic_safe_id(&device)
            );

            if !current_topic_set.contains(&legacy_topic) {
                topics.push(legacy_topic);
            }
        }

        Ok(topics)
    }

    pub async fn publish<T: AsRef<str> + std::fmt::Display, P: AsRef<[u8]> + std::fmt::Display>(
        &self,
        topic: T,
        payload: P,
    ) -> anyhow::Result<()> {
        self.publish_with_options(topic, payload, true).await
    }

    pub async fn publish_retained<
        T: AsRef<str> + std::fmt::Display,
        P: AsRef<[u8]> + std::fmt::Display,
    >(
        &self,
        topic: T,
        payload: P,
    ) -> anyhow::Result<()> {
        self.publish_with_options(topic, payload, true).await
    }

    async fn publish_with_options<
        T: AsRef<str> + std::fmt::Display,
        P: AsRef<[u8]> + std::fmt::Display,
    >(
        &self,
        topic: T,
        payload: P,
        retain: bool,
    ) -> anyhow::Result<()> {
        log::trace!("{topic} -> {payload}");
        let topic = topic.to_string();
        let payload_string = payload.to_string();
        self.record_config_topic(&topic);
        match &self.backend {
            HassClientBackend::Mqtt(client) => {
                client
                    .publish(&topic, payload_string.as_bytes(), QoS::AtLeastOnce, retain)
                    .await?;
            }
            #[cfg(test)]
            HassClientBackend::Capture(messages) => {
                messages.lock().push((topic, payload_string));
            }
        }
        Ok(())
    }

    pub async fn publish_obj<T: AsRef<str> + std::fmt::Display, P: Serialize>(
        &self,
        topic: T,
        payload: P,
    ) -> anyhow::Result<()> {
        self.publish_obj_with_options(topic, payload, true).await
    }

    pub async fn publish_obj_retained<T: AsRef<str> + std::fmt::Display, P: Serialize>(
        &self,
        topic: T,
        payload: P,
    ) -> anyhow::Result<()> {
        self.publish_obj_with_options(topic, payload, true).await
    }

    async fn publish_obj_with_options<T: AsRef<str> + std::fmt::Display, P: Serialize>(
        &self,
        topic: T,
        payload: P,
        retain: bool,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&payload)?;
        log::trace!("{topic} -> {payload}");
        let topic = topic.to_string();
        self.record_config_topic(&topic);
        match &self.backend {
            HassClientBackend::Mqtt(client) => {
                client
                    .publish(&topic, payload.as_bytes(), QoS::AtLeastOnce, retain)
                    .await?;
            }
            #[cfg(test)]
            HassClientBackend::Capture(messages) => {
                messages.lock().push((topic, payload));
            }
        }
        Ok(())
    }

    pub async fn advise_hass_of_light_state(
        &self,
        device: &ServiceDevice,
        state: &StateHandle,
    ) -> anyhow::Result<()> {
        let mut entities = EntityList::new();
        enumerate_entities_for_device(device, state, &mut entities).await?;
        entities.notify_state(self).await?;

        Ok(())
    }
}

fn discovery_topics_cache_path() -> PathBuf {
    let mut path = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    path.push(DISCOVERY_TOPICS_CACHE_FILE);
    path
}

fn load_discovery_topics_cache() -> Vec<String> {
    let path = discovery_topics_cache_path();
    let Ok(data) = fs::read_to_string(path) else {
        return vec![];
    };

    serde_json::from_str(&data).unwrap_or_default()
}

fn save_discovery_topics_cache(topics: &[String]) -> anyhow::Result<()> {
    let path = discovery_topics_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec(topics)?)?;
    Ok(())
}

pub fn topic_safe_string(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        if c == ':' || c == ' ' || c == '\\' || c == '/' || c == '\'' || c == '"' {
            result.push('_');
        } else {
            result.push(c.to_ascii_lowercase());
        }
    }
    result
}

pub fn topic_safe_id(device: &ServiceDevice) -> String {
    topic_safe_id_str(&device.id)
}

pub fn topic_safe_id_str(id: &str) -> String {
    let mut id = id.to_string();
    id.retain(|c| c != ':');
    id.retain(|c| c != ' ');
    id
}

pub fn switch_instance_state_topic(device: &ServiceDevice, instance: &str) -> String {
    format!(
        "gv2mqtt/switch/{id}/{instance}/state",
        id = topic_safe_id(device)
    )
}

pub fn light_state_topic(device: &ServiceDevice) -> String {
    format!("gv2mqtt/light/{id}/state", id = topic_safe_id(device))
}

pub fn light_segment_state_topic(device: &ServiceDevice, segment: u32) -> String {
    format!(
        "gv2mqtt/light/{id}/state/{segment}",
        id = topic_safe_id(device)
    )
}

/// Global bridge availability topic, used as last-will
pub fn availability_topic() -> String {
    "gv2mqtt/availability".to_string()
}

/// Per-device availability topic
pub fn device_availability_topic(device: &ServiceDevice) -> String {
    format!("gv2mqtt/{}/availability", topic_safe_id(device))
}

/// Build availability entries for a device-bound entity.
/// Includes both the global bridge topic and the per-device topic.
/// Entity is available only when both the bridge AND the device are online.
pub fn device_availability_entries(
    device: &ServiceDevice,
) -> (Vec<crate::hass_mqtt::base::AvailabilityEntry>, Option<String>) {
    use crate::hass_mqtt::base::AvailabilityEntry;
    (
        vec![
            AvailabilityEntry {
                topic: availability_topic(),
            },
            AvailabilityEntry {
                topic: device_availability_topic(device),
            },
        ],
        Some("all".to_string()),
    )
}

pub fn oneclick_topic() -> String {
    "gv2mqtt/oneclick".to_string()
}

pub fn purge_cache_topic() -> String {
    "gv2mqtt/purge-caches".to_string()
}

#[derive(Deserialize)]
pub struct IdParameter {
    pub id: String,
}

/// Someone clicked the "Request Platform API State" button
async fn mqtt_request_platform_data(
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_read_only(&id).await?;
    log::info!("Request Platform API State for {device}");
    if !state.poll_platform_api(&device).await? {
        log::warn!("Unable to poll platform API for {device}");
    }
    Ok(())
}

#[derive(Deserialize, Debug, Clone)]
struct HassLightCommand {
    state: String,
    color_temp: Option<u32>,
    color: Option<DeviceColor>,
    effect: Option<String>,
    brightness: Option<u8>,
}

/// HASS is sending a command to a light
async fn mqtt_group_light_command(
    Payload(payload): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("Group command for {id}: {payload}");

    // Find the group members from config
    let groups = crate::service::device_config::get_groups();
    let group = groups
        .iter()
        .find(|(gid, _)| gid.replace(' ', "_").eq_ignore_ascii_case(&id))
        .map(|(_, g)| g)
        .ok_or_else(|| anyhow::anyhow!("group '{id}' not found"))?;

    // Fan out the command to all member devices in parallel
    let mut handles = vec![];
    for member_id in &group.members {
        let state = state.clone();
        let member_id = member_id.clone();
        let payload = payload.clone();
        handles.push(tokio::spawn(async move {
            match state.resolve_device_for_control(&member_id).await {
                Ok(device) => {
                    // Re-parse command for each device and apply
                    let command: HassLightCommand = match serde_json::from_str(&payload) {
                        Ok(cmd) => cmd,
                        Err(err) => {
                            log::warn!("Failed to parse group command for {member_id}: {err:#}");
                            return;
                        }
                    };

                    if command.state == "OFF" {
                        let _ = state.device_light_power_on(&device, false).await;
                    } else {
                        if let Some(brightness) = command.brightness {
                            let _ = state.device_set_brightness(&device, brightness).await;
                        }
                        if let Some(color) = &command.color {
                            let _ = state
                                .device_set_color_rgb(&device, color.r, color.g, color.b)
                                .await;
                        }
                        if let Some(kelvin) = command.color_temp {
                            let _ = state.device_set_color_temperature(&device, kelvin).await;
                        }
                        let _ = state.device_light_power_on(&device, true).await;
                    }
                }
                Err(err) => {
                    log::warn!("Group member {member_id} not found: {err:#}");
                }
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

async fn mqtt_light_command(
    Payload(payload): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;

    let command: HassLightCommand = serde_json::from_str(&payload)?;
    log::info!("Command for {device}: {payload}");

    let is_light = device.device_type() == DeviceType::Light;

    if command.state == "OFF" {
        if is_light {
            state
                .device_light_power_on(&device, false)
                .await
                .context("mqtt_light_command: state.device_power_on")?;
        } else {
            state
                .device_set_brightness(&device, 0)
                .await
                .context("mqtt_light_command: state.device_set_brightness")?;
        }
    } else {
        let mut power_on = true;

        if let Some(brightness) = command.brightness {
            state
                .device_set_brightness(&device, brightness)
                .await
                .context("mqtt_light_command: state.device_set_brightness")?;
            power_on = false;
        }

        if let Some(effect) = &command.effect {
            state
                .device_set_scene(&device, effect)
                .await
                .context("mqtt_light_command: state.device_set_scene")?;
            // It doesn't make sense to vary color properties
            // at the same time as the scene properties, so
            // ignore those.
            // Brightness, set above, is ok.
            return Ok(());
        }

        if let Some(color) = &command.color {
            state
                .device_set_color_rgb(&device, color.r, color.g, color.b)
                .await
                .context("mqtt_light_command: state.device_set_color_rgb")?;
            power_on = false;
        }
        if let Some(color_temp) = command.color_temp {
            state
                .device_set_color_temperature(&device, mired_to_kelvin(color_temp))
                .await
                .context("mqtt_light_command: state.device_set_color_temperature")?;
            power_on = false;
        }

        if power_on {
            if is_light {
                state
                    .device_light_power_on(&device, true)
                    .await
                    .context("mqtt_light_command: state.device_power_on")?;
            } else if command.brightness.is_none() {
                // The device is not primarily a light and we don't have
                // a guaranteed way to power it on without setting the
                // brightness to something, and we know we didn't set
                // the brightness just now, so let's turn it on 100%
                state
                    .device_set_brightness(&device, 100)
                    .await
                    .context("mqtt_light_command: state.device_set_brightness")?;
            }
        }
    }

    Ok(())
}

#[derive(Deserialize)]
struct IdAndSeg {
    id: String,
    segment: String,
}

async fn mqtt_light_segment_command(
    Payload(payload): Payload<String>,
    Params(IdAndSeg { id, segment }): Params<IdAndSeg>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;
    let segment: u32 = segment.parse()?;

    let command: HassLightCommand = from_json(&payload)?;
    log::info!("Command for {device} segment {segment}: {payload}");

    if let Some(client) = state.get_platform_client().await {
        let info = device
            .http_device_info
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("HTTP device info is missing"))?;

        log::info!("Using Platform API to control {device} segment");

        if let Some(brightness) = command.brightness {
            client
                .set_segment_brightness(info, segment, brightness)
                .await?;
        } else if command.state == "OFF" {
            // Do nothing here. We used to set brightness to zero,
            // but it is problematic:
            // * Some devices don't have a 0
            // * Setting it to 0 will power up the rest of the device,
            //   so if HASS is turning off all lights in an area, the
            //   effect is that they will turn off and then immediate
            //   on again when there are segments involved
            // client.set_segment_brightness(&info, segment, 0).await?;
        }
        if let Some(color) = &command.color {
            client
                .set_segment_rgb(info, segment, color.r, color.g, color.b)
                .await?;
        }
    } else if let Some(lan_dev) = &device.lan_device {
        // LAN fallback for segment control via ptReal binary protocol
        log::info!("Using LAN API to control {device} segment {segment}");
        if command.state == "OFF" {
            // Turn off segment by setting it to black via LAN ptReal
            lan_dev
                .send_segment_color_rgb(segment, 0, 0, 0)
                .await?;
        }
        if let Some(color) = &command.color {
            lan_dev
                .send_segment_color_rgb(segment, color.r, color.g, color.b)
                .await?;
        }
    } else {
        anyhow::bail!("set segments for {device}: no API available for segment control");
    }

    Ok(())
}

async fn mqtt_purge_caches(State(state): State<StateHandle>) -> anyhow::Result<()> {
    log::info!("mqtt_purge_caches");
    crate::cache::purge_cache()?;
    state
        .get_hass_client()
        .await
        .expect("have hass client")
        .register_with_hass(&state)
        .await
        .context("register_with_hass")
}

async fn mqtt_bridge_request_health(State(state): State<StateHandle>) -> anyhow::Result<()> {
    log::info!("mqtt bridge request: health");
    crate::service::ext_health::publish_bridge_health(&state).await;
    Ok(())
}

async fn mqtt_bridge_request_restart(State(_state): State<StateHandle>) -> anyhow::Result<()> {
    log::info!("mqtt bridge request: restart");
    // Publish response before exiting
    if let Some(hass) = _state.get_hass_client().await {
        let _ = hass
            .publish_retained(
                "gv2mqtt/bridge/response/restart",
                r#"{"status":"ok"}"#,
            )
            .await;
    }
    // Give MQTT time to publish the response
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    std::process::exit(1);
}

async fn mqtt_bridge_request_devices(State(state): State<StateHandle>) -> anyhow::Result<()> {
    log::info!("mqtt bridge request: devices");
    crate::service::ext_health::publish_bridge_devices(&state).await;
    Ok(())
}

async fn mqtt_bridge_request_config_reload(
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("mqtt bridge request: config_reload");
    crate::service::device_config::load_device_config();
    if let Some(hass) = state.get_hass_client().await {
        let _ = hass
            .publish_retained(
                "gv2mqtt/bridge/response/config_reload",
                r#"{"status":"ok"}"#,
            )
            .await;
        hass.re_register(&state).await?;
    }
    Ok(())
}

async fn mqtt_bridge_request_log_level(
    Payload(level): Payload<String>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("mqtt bridge request: log_level -> {level}");
    // Update the log filter at runtime
    log::set_max_level(
        level
            .parse()
            .unwrap_or(log::LevelFilter::Info),
    );
    if let Some(hass) = state.get_hass_client().await {
        let _ = hass
            .publish_retained(
                "gv2mqtt/bridge/response/log_level",
                &format!(r#"{{"status":"ok","level":"{level}"}}"#),
            )
            .await;
    }
    Ok(())
}

async fn mqtt_bridge_request_cache_purge(
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("mqtt bridge request: cache_purge");
    crate::cache::purge_cache()?;
    if let Some(hass) = state.get_hass_client().await {
        let _ = hass
            .publish_retained(
                "gv2mqtt/bridge/response/cache_purge",
                r#"{"status":"ok"}"#,
            )
            .await;
    }
    state
        .get_hass_client()
        .await
        .expect("have hass client")
        .register_with_hass(&state)
        .await
        .context("register_with_hass after cache purge")
}

async fn mqtt_oneclick(
    Payload(name): Payload<String>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("mqtt_oneclick: {name}");

    let undoc = match state.get_undoc_client().await {
        Some(client) => client,
        None => {
            let msg = "One-click failed: Undoc API client is not available. \
                       Configure govee_email and govee_password to enable one-click scenes.";
            log::error!("{msg}");
            if let Some(hass) = state.get_hass_client().await {
                let _ = hass
                    .publish("gv2mqtt/bridge/error", msg)
                    .await;
            }
            anyhow::bail!("{msg}");
        }
    };

    let items = match undoc.parse_one_clicks().await {
        Ok(items) => items,
        Err(err) => {
            let msg = format!(
                "One-click '{name}' failed: could not fetch one-click list: {err:#}. \
                 This may be a rate limit or network issue."
            );
            log::error!("{msg}");
            if let Some(hass) = state.get_hass_client().await {
                let _ = hass.publish("gv2mqtt/bridge/error", &msg).await;
            }
            anyhow::bail!("{msg}");
        }
    };

    let item = match items.iter().find(|item| item.name == name) {
        Some(item) => item,
        None => {
            let available: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
            let msg = format!(
                "One-click '{name}' not found. Available: {available:?}. \
                 Try purging caches if you recently created this scene in the Govee app."
            );
            log::error!("{msg}");
            if let Some(hass) = state.get_hass_client().await {
                let _ = hass.publish("gv2mqtt/bridge/error", &msg).await;
            }
            anyhow::bail!("{msg}");
        }
    };

    let iot = match state.get_iot_client().await {
        Some(client) => client,
        None => {
            let msg = "One-click failed: AWS IoT client is not connected. \
                       One-click scenes require the IoT connection (govee_email + govee_password).";
            log::error!("{msg}");
            if let Some(hass) = state.get_hass_client().await {
                let _ = hass.publish("gv2mqtt/bridge/error", msg).await;
            }
            anyhow::bail!("{msg}");
        }
    };

    match iot.activate_one_click(item).await {
        Ok(()) => {
            log::info!("One-click '{name}' activated successfully");
            Ok(())
        }
        Err(err) => {
            let msg = format!("One-click '{name}' failed to activate: {err:#}");
            log::error!("{msg}");
            if let Some(hass) = state.get_hass_client().await {
                let _ = hass.publish("gv2mqtt/bridge/error", &msg).await;
            }
            Err(err)
        }
    }
}

#[derive(Deserialize)]
struct IdAndInst {
    id: String,
    instance: String,
}

async fn mqtt_switch_command(
    Payload(command): Payload<String>,
    Params(IdAndInst { id, instance }): Params<IdAndInst>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("{instance} for {id}: {command}");
    let device = state.resolve_device_for_control(&id).await?;

    let on = match command.as_str() {
        "ON" | "on" => true,
        "OFF" | "off" => false,
        _ => anyhow::bail!("invalid {command} for {id}"),
    };

    if instance == "powerSwitch" {
        state.device_power_on(&device, on).await?;
    } else if let Some(client) = state.get_platform_client().await {
        if let Some(http_dev) = &device.http_device_info {
            client.set_toggle_state(http_dev, &instance, on).await?;
        } else {
            anyhow::bail!("No platform state available to set {id} {instance} to {on}");
        }
    } else {
        anyhow::bail!("Don't know how to {command} for {id} {instance}!");
    }

    Ok(())
}

pub fn mired_to_kelvin(mired: u32) -> u32 {
    1000000u32.checked_div(mired).unwrap_or(0)
}

pub fn kelvin_to_mired(kelvin: u32) -> u32 {
    1000000u32.checked_div(kelvin).unwrap_or(0)
}

/// HASS is advising us that its status has changed
async fn mqtt_homeassitant_status(
    Payload(status): Payload<String>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let client = state
        .get_hass_client()
        .await
        .expect("hass client to be present");

    log::info!("Home Assistant status changed: {status}, waiting {HASS_REGISTER_DELAY:?} before re-registering entities");
    tokio::time::sleep(HASS_REGISTER_DELAY).await;

    client.register_with_hass(&state).await?;

    Ok(())
}

async fn run_mqtt_loop(
    state: StateHandle,
    subscriber: Receiver<Event>,
    client: Client,
) -> anyhow::Result<()> {
    // Give LAN disco a chance to get current state before
    // we register with hass
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    async fn rebuild_router(
        client: &Client,
        state: &StateHandle,
    ) -> anyhow::Result<Arc<MqttRouter<StateHandle>>> {
        let disco_prefix = state.get_hass_disco_prefix().await;
        let mut router: MqttRouter<StateHandle> = MqttRouter::new(client.clone());

        router
            .route(format!("{disco_prefix}/status"), mqtt_homeassitant_status)
            .await?;

        router
            .route("gv2mqtt/light/:id/command", mqtt_light_command)
            .await?;
        router
            .route("gv2mqtt/group/:id/command", mqtt_group_light_command)
            .await?;
        router
            .route(
                "gv2mqtt/light/:id/command/:segment",
                mqtt_light_segment_command,
            )
            .await?;
        router
            .route("gv2mqtt/switch/:id/command/:instance", mqtt_switch_command)
            .await?;

        router.route(oneclick_topic(), mqtt_oneclick).await?;
        router.route(purge_cache_topic(), mqtt_purge_caches).await?;
        router
            .route(
                "gv2mqtt/bridge/request/health",
                mqtt_bridge_request_health,
            )
            .await?;
        router
            .route(
                "gv2mqtt/bridge/request/restart",
                mqtt_bridge_request_restart,
            )
            .await?;
        router
            .route(
                "gv2mqtt/bridge/request/cache_purge",
                mqtt_bridge_request_cache_purge,
            )
            .await?;
        router
            .route(
                "gv2mqtt/bridge/request/devices",
                mqtt_bridge_request_devices,
            )
            .await?;
        router
            .route(
                "gv2mqtt/bridge/request/config_reload",
                mqtt_bridge_request_config_reload,
            )
            .await?;
        router
            .route(
                "gv2mqtt/bridge/request/log_level",
                mqtt_bridge_request_log_level,
            )
            .await?;
        router
            .route(
                "gv2mqtt/:id/request-platform-data",
                mqtt_request_platform_data,
            )
            .await?;
        router
            .route(
                "gv2mqtt/number/:id/command/:mode_name/:work_mode",
                mqtt_number_command,
            )
            .await?;
        router
            .route("gv2mqtt/humidifier/:id/set-mode", mqtt_device_set_work_mode)
            .await?;
        router
            .route("gv2mqtt/:id/set-work-mode", mqtt_device_set_work_mode)
            .await?;
        router
            .route(
                "gv2mqtt/:id/set-capability-option/:instance",
                mqtt_set_capability_option,
            )
            .await?;
        router
            .route("gv2mqtt/:id/set-music-mode", mqtt_set_music_mode)
            .await?;
        router
            .route(
                "gv2mqtt/:id/set-music-sensitivity",
                mqtt_set_music_sensitivity,
            )
            .await?;
        router
            .route(
                "gv2mqtt/:id/set-music-auto-color",
                mqtt_set_music_auto_color,
            )
            .await?;
        router
            .route(
                "gv2mqtt/humidifier/:id/set-target",
                mqtt_humidifier_set_target,
            )
            .await?;
        router
            .route(
                "gv2mqtt/:id/set-temperature/:instance/:units",
                mqtt_set_temperature,
            )
            .await?;
        router
            .route("gv2mqtt/:id/set-mode-scene", mqtt_set_mode_scene)
            .await?;

        tokio::time::sleep(HASS_REGISTER_DELAY).await;
        state
            .get_hass_client()
            .await
            .expect("have hass client")
            .register_with_hass(state)
            .await
            .context("register_with_hass")?;

        Ok(Arc::new(router))
    }

    let mut router = rebuild_router(&client, &state).await?;
    let mut need_rebuild = false;

    while let Ok(event) = subscriber.recv().await {
        match event {
            Event::Message(msg) => {
                let router = router.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = router.dispatch(msg.clone(), state.clone()).await {
                        log::error!("While dispatching {msg:?}: {err:#}");
                    }
                });
            }
            Event::Disconnected(reason) => {
                log::warn!("MQTT disconnected with reason={reason}");
                need_rebuild = true;
            }
            Event::Connected(status) => {
                log::info!("MQTT connected with status={status}");
                if need_rebuild {
                    router = rebuild_router(&client, &state).await?;
                }
            }
        }
    }

    log::info!("subscriber.recv loop terminated");

    Ok(())
}

pub async fn spawn_hass_integration(
    state: StateHandle,
    args: &HassArguments,
) -> anyhow::Result<()> {
    // Client IDs must not contain '/' — Mosquitto 7+ rejects them with
    // "dangerous client id" (see issues #659, #661).
    let client = Client::with_id(
        &format!("govee2mqtt-{}", uuid::Uuid::new_v4().simple()),
        true,
    )?;

    state.set_temperature_scale(args.temperature_scale()?).await;

    let mqtt_host = args.mqtt_host()?;
    let mqtt_username = args.mqtt_username()?;
    let mqtt_password = args.mqtt_password()?;
    let mqtt_port = args.mqtt_port()?;

    client.set_last_will(availability_topic(), "offline", QoS::AtLeastOnce, true)?;

    if mqtt_username.is_some() != mqtt_password.is_some() {
        log::error!(
            "MQTT username and password either both need to be set, or both need to be unset"
        );
    }
    client.set_username_and_password(mqtt_username.as_deref(), mqtt_password.as_deref())?;

    let mut connected = false;
    for _ in 0..30 {
        log::info!("Attempting connection to mqtt broker {mqtt_host}:{mqtt_port}...");
        match client
            .connect(
                &mqtt_host,
                mqtt_port.into(),
                Duration::from_secs(120),
                args.mqtt_bind_address.as_deref(),
            )
            .await
        {
            Ok(status) => {
                log::info!("Connected to mqtt broker {mqtt_host}:{mqtt_port}, status={status}");
                connected = true;
                break;
            }
            Err(err) => {
                log::error!("Failed to connect to mqtt broker {mqtt_host}:{mqtt_port}: {err:#}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }

    anyhow::ensure!(
        connected,
        "Failed to connect to mqtt broker after several attempts"
    );

    let subscriber = client.subscriber().expect("to own the subscriber");

    state
        .set_hass_client(HassClient::from_client(client.clone()))
        .await;

    let disco_prefix = args.hass_discovery_prefix.clone();
    state.set_hass_disco_prefix(disco_prefix).await;

    tokio::spawn(async move {
        let res = run_mqtt_loop(state, subscriber, client).await;
        if let Err(err) = res {
            log::error!("run_mqtt_loop: {err:#}");
            log::error!("FATAL: hass integration will not function.");
            log::error!("Pausing for 30 seconds before terminating.");
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            std::process::exit(1);
        } else {
            log::info!("run_mqtt_loop exited. We should do something to shutdown gracefully here");
            std::process::exit(1);
        }
    });

    Ok(())
}

pub fn camel_case_to_space_separated(camel: &str) -> String {
    let mut chars = camel.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut result = String::new();
    result.extend(first.to_uppercase());
    for c in chars {
        if c.is_uppercase() {
            result.push(' ');
        }
        result.push(c);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camel_case_to_space_separated() {
        assert_eq!(camel_case_to_space_separated("powerSwitch"), "Power Switch");
        assert_eq!(
            camel_case_to_space_separated("oscillationToggle"),
            "Oscillation Toggle"
        );
    }

    #[test]
    fn test_camel_case_chinese_no_panic() {
        assert_eq!(
            camel_case_to_space_separated("用于三灯头中的第二个"),
            "用于三灯头中的第二个"
        );
    }

    #[test]
    fn test_camel_case_empty() {
        assert_eq!(camel_case_to_space_separated(""), "");
    }

    #[test]
    fn test_camel_case_emoji() {
        assert_eq!(camel_case_to_space_separated("🔥lightMode"), "🔥light Mode");
    }
}
