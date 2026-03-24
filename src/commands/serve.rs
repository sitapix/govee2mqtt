use crate::lan_api::Client as LanClient;
use crate::platform_api::{DeviceParameters, GoveeApiClient};
use crate::service::device::Device;
use crate::service::ext_availability::AvailabilityExtension;
use crate::service::ext_config_reload::ConfigReloadExtension;
use crate::service::ext_discovery::DiscoveryExtension;
use crate::service::ext_health::HealthExtension;
use crate::service::extension::ExtensionManager;
use crate::service::hass::spawn_hass_integration;
use crate::service::http::run_http_server;
use crate::service::iot::start_iot_client;
use crate::service::state::StateHandle;
use crate::undoc_api::GoveeUndocumentedApi;
use crate::version_info::govee_version;
use crate::UndocApiArguments;
use anyhow::Context;
use chrono::Utc;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

/// Default poll interval in seconds. Override with GOVEE_POLL_INTERVAL env var.
/// Per-device rate at 120s: 720 req/day. With many devices that can exceed
/// the 10,000/day Platform API limit — but IoT/Push/LAN state updates skip
/// the Platform API poll, so only devices without those channels consume quota.
/// Users with 14+ non-IoT/LAN devices should increase this (e.g. 900).
pub static POLL_INTERVAL: Lazy<chrono::Duration> = Lazy::new(|| {
    parse_poll_interval(std::env::var("GOVEE_POLL_INTERVAL").ok().as_deref())
});

/// Parse poll interval from an optional string value, falling back to 120s.
/// Clamps to a minimum of 30s to prevent API abuse and broken online detection.
fn parse_poll_interval(env_val: Option<&str>) -> chrono::Duration {
    let secs = env_val
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|&s| s >= 30)
        .unwrap_or(120);
    log::info!("Poll interval: {secs}s");
    chrono::Duration::seconds(secs)
}

#[derive(clap::Parser, Debug)]
pub struct ServeCommand {
    /// The port on which the HTTP API will listen
    #[arg(long, default_value_t = 8056)]
    http_port: u16,
}

async fn poll_single_device(state: &StateHandle, device: &Device) -> anyhow::Result<()> {
    let now = Utc::now();

    if device.is_ble_only_device() == Some(true) {
        // We can't poll this device, we have no ble support
        return Ok(());
    }

    // Collect the device status via the LAN API, if possible.
    // Skip LAN polling when the device is OFF to prevent firmware-induced
    // flashing on some devices (e.g. H6061 Glide Hexa).
    // <https://github.com/wez/govee2mqtt/issues/250>
    let is_off = device
        .device_state()
        .map(|s| !s.on)
        .unwrap_or(false);

    if !is_off {
        if let Some(lan_device) = &device.lan_device {
            if let Some(client) = state.get_lan_client().await {
                if let Ok(status) = client.query_status(lan_device).await {
                    state
                        .device_mut(&lan_device.sku, &lan_device.device)
                        .await
                        .set_lan_device_status(status);
                    state.notify_of_state_change(&lan_device.device).await.ok();
                }
            }
        }
    }

    let poll_interval = device.preferred_poll_interval();

    let can_update = match &device.last_polled {
        None => true,
        Some(last) => now - last > poll_interval,
    };

    if !can_update {
        return Ok(());
    }

    let device_state = device.device_state();
    let needs_update = match &device_state {
        None => true,
        Some(state) => now - state.updated > poll_interval,
    };

    if !needs_update {
        return Ok(());
    }

    let needs_platform = device.needs_platform_poll();

    // Don't interrogate via HTTP if we can use the LAN.
    // If we have LAN and the device is stale, it is likely
    // offline and there is little sense in burning up request
    // quota to the platform API for it
    if device.lan_device.is_some() && !needs_platform {
        log::trace!("LAN-available device {device} needs a status update; it's likely offline.");
        return Ok(());
    }

    if !needs_platform && state.poll_iot_api(device).await? {
        return Ok(());
    }

    state.poll_platform_api(device).await?;

    Ok(())
}

async fn periodic_state_poll(
    state: StateHandle,
    extensions: Arc<ExtensionManager>,
) -> anyhow::Result<()> {
    sleep(Duration::from_secs(20)).await;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    loop {
        let devices = state.devices().await;
        let mut set = tokio::task::JoinSet::new();
        for d in devices {
            let state = state.clone();
            let sem = semaphore.clone();
            set.spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore closed");
                if let Err(err) = poll_single_device(&state, &d).await {
                    log::error!("while polling {d}: {err:#}");
                }
            });
        }
        while let Some(result) = set.join_next().await {
            if let Err(e) = result {
                log::error!("polling task panicked: {e}");
            }
        }

        extensions.tick_all(&state).await;

        sleep(Duration::from_secs(30)).await;
    }
}

async fn enumerate_devices_via_platform_api(
    state: StateHandle,
    client: Option<GoveeApiClient>,
) -> anyhow::Result<()> {
    let client = match client {
        Some(client) => client,
        None => match state.get_platform_client().await {
            Some(client) => client,
            None => return Ok(()),
        },
    };

    log::info!("Querying Platform API for device list...");
    let devices = client.get_devices().await?;
    log::info!("Platform API returned {} devices", devices.len());
    for info in devices {
        let mut device = state.device_mut(&info.sku, &info.device).await;
        device.set_http_device_info(info);
    }
    Ok(())
}

async fn enumerate_devices_via_undo_api(
    state: StateHandle,
    client: Option<GoveeUndocumentedApi>,
    args: &UndocApiArguments,
) -> anyhow::Result<()> {
    let (client, needs_start) = match client {
        Some(client) => (client, true),
        None => match state.get_undoc_client().await {
            Some(client) => (client, false),
            None => return Ok(()),
        },
    };

    log::info!("Querying undocumented API for device + room list...");
    let acct = client.login_account_cached().await?;
    let info = client.get_device_list(&acct.token).await?;
    let mut group_by_id = HashMap::new();
    for group in info.groups {
        group_by_id.insert(group.group_id, group.group_name);
    }
    for entry in info.devices {
        let mut device = state.device_mut(&entry.sku, &entry.device).await;
        let room_name = group_by_id.get(&entry.group_id).map(|name| name.as_str());
        device.set_undoc_device_info(entry, room_name);
    }

    if needs_start {
        start_iot_client(args, state.clone(), Some(acct)).await?;
    }
    Ok(())
}

const ISSUE_76_EXPLANATION: &str = "Startup cannot automatically continue because entity names\n\
    could become inconsistent especially across frequent similar\n\
    intermittent issues if/as they occur on an ongoing basis.\n\
    Please see https://github.com/wez/govee2mqtt/issues/76\n\
    A workaround is to remove the Govee API credentials from your\n\
    configuration, which will cause this govee2mqtt to use only\n\
    the LAN API. Two consequences of that will be loss of control\n\
    over devices that do not support the LAN API, and also devices\n\
    changing entity ID to less descriptive names due to lack of\n\
    metadata availability via the LAN API.";

impl ServeCommand {
    pub async fn run(&self, args: &crate::Args) -> anyhow::Result<()> {
        log::info!("Starting service. version {}", govee_version());
        crate::service::device_config::load_device_config();
        crate::service::scene_database::load_scene_databases();
        let state = Arc::new(crate::service::state::State::new());

        Self::discover_devices(&state, args).await?;
        Self::start_lan_discovery(&state, args).await?;
        Self::log_discovered_devices(&state).await;
        Self::start_services(self.http_port, &state, args).await
    }

    /// Run API discovery, falling back to cached device database on failure.
    async fn discover_devices(
        state: &StateHandle,
        args: &crate::Args,
    ) -> anyhow::Result<()> {
        let mut device_db = crate::service::device_database::load_device_database();
        let mut api_discovery_failed = false;

        if let Ok(client) = args.api_args.api_client() {
            if let Err(err) =
                enumerate_devices_via_platform_api(state.clone(), Some(client.clone())).await
            {
                let err_str = format!("{err:#}");
                if err_str.contains("dns error") || err_str.contains("Name does not resolve") {
                    log::error!(
                        "DNS resolution failed for Govee API. Check your network connection \
                         and DNS settings. If running in Docker, ensure DNS is configured. \
                         Error: {err:#}"
                    );
                } else if err_str.contains("connect") || err_str.contains("timeout") {
                    log::error!(
                        "Cannot reach Govee API servers. Check your internet connection. \
                         Error: {err:#}"
                    );
                } else {
                    log::error!("Error during initial platform API discovery: {err:#}");
                }
                api_discovery_failed = true;
            } else {
                state.set_platform_client(client).await;

                let state = state.clone();
                tokio::spawn(async move {
                    loop {
                        sleep(Duration::from_secs(600)).await;
                        if let Err(err) =
                            enumerate_devices_via_platform_api(state.clone(), None).await
                        {
                            log::error!(
                                "Error during periodic platform API discovery: {err:#}"
                            );
                        }
                    }
                });
            }
        }

        if let Ok(client) = args.undoc_args.api_client() {
            if let Err(err) = enumerate_devices_via_undo_api(
                state.clone(),
                Some(client.clone()),
                &args.undoc_args,
            )
            .await
            {
                log::error!("Error during initial undoc API discovery: {err:#}");
                api_discovery_failed = true;
            } else {
                state.set_undoc_client(client).await;
            }

            let state = state.clone();
            let args = args.undoc_args.clone();
            tokio::spawn(async move {
                loop {
                    sleep(Duration::from_secs(600)).await;
                    if let Err(err) =
                        enumerate_devices_via_undo_api(state.clone(), None, &args).await
                    {
                        log::error!("Error during periodic undoc API discovery: {err:#}");
                    }
                }
            });
        }

        if !api_discovery_failed {
            let devices = state.devices().await;
            if !devices.is_empty() {
                crate::service::device_database::update_database_from_devices(
                    &mut device_db,
                    &devices,
                );
                if let Err(err) = crate::service::device_database::save_device_database(&device_db)
                {
                    log::warn!("Failed to save device database: {err:#}");
                }
            }
        } else if !device_db.devices.is_empty() {
            log::warn!(
                "API discovery failed. Using cached device database ({} devices). \
                 Some features may be limited.",
                device_db.devices.len()
            );
            for persisted in device_db.devices.values() {
                let mut device = state
                    .device_mut(&persisted.sku, &persisted.device_id)
                    .await;
                device.set_http_device_info(crate::platform_api::HttpDeviceInfo {
                    sku: persisted.sku.clone(),
                    device: persisted.device_id.clone(),
                    device_name: persisted.name.clone(),
                    device_type: Default::default(),
                    capabilities: vec![],
                });
            }
        } else if api_discovery_failed {
            anyhow::bail!(
                "API discovery failed and no cached device database exists.\n{ISSUE_76_EXPLANATION}"
            );
        }

        Ok(())
    }

    /// Start LAN UDP discovery and wait for initial device probes.
    async fn start_lan_discovery(
        state: &StateHandle,
        args: &crate::Args,
    ) -> anyhow::Result<()> {
        let options = args.lan_disco_args.to_disco_options()?;
        if options.is_empty() {
            return Ok(());
        }

        log::info!("Starting LAN discovery");
        let state = state.clone();
        let (client, mut scan) = LanClient::new(options).await?;

        state.set_lan_client(client.clone()).await;

        tokio::spawn(async move {
            while let Some(lan_device) = scan.recv().await {
                log::trace!("LAN disco: {lan_device:?}");
                state
                    .device_mut(&lan_device.sku, &lan_device.device)
                    .await
                    .set_lan_device(lan_device.clone());

                let state = state.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    if let Ok(status) = client.query_status(&lan_device).await {
                        state
                            .device_mut(&lan_device.sku, &lan_device.device)
                            .await
                            .set_lan_device_status(status);

                        log::trace!("LAN disco: update and notify {}", lan_device.device);
                        state.notify_of_state_change(&lan_device.device).await.ok();
                    }
                });
            }
        });

        log::info!("Waiting 10 seconds for LAN API discovery");
        sleep(Duration::from_secs(10)).await;

        Ok(())
    }

    /// Log all discovered devices with their API capabilities.
    async fn log_discovered_devices(state: &StateHandle) {
        let device_count = state.devices().await.len();
        log::info!("Discovered {device_count} devices:");
        for device in state.devices().await {
            log::info!("{device}");
            if let Some(lan) = &device.lan_device {
                log::info!("  LAN API: ip={:?}", lan.ip);
            }
            if let Some(http_info) = &device.http_device_info {
                let kind = &http_info.device_type;
                let rgb = http_info.supports_rgb();
                let bright = http_info.supports_brightness();
                let color_temp = http_info.get_color_temperature_range();
                let segment_rgb = http_info.supports_segmented_rgb();
                log::info!(
                    "  Platform API: {kind}. supports_rgb={rgb} supports_brightness={bright}"
                );
                log::info!("                color_temp={color_temp:?} segment_rgb={segment_rgb:?}");

                for instance in ["diyScene", "snapshot"] {
                    if let Some(cap) = http_info.capability_by_instance(instance) {
                        if matches!(
                            &cap.parameters,
                            Some(DeviceParameters::Enum { options }) if options.is_empty()
                        ) {
                            log::warn!(
                                "  Platform API advertises {instance}, but returns no options. \
                                 Dedicated Home Assistant controls for it cannot be published \
                                 until Govee provides usable option data."
                            );
                        }
                    }
                }
                log::trace!("{http_info:#?}");
            }
            if let Some(undoc) = &device.undoc_device_info {
                let room = &undoc.room_name;
                let supports_iot = undoc.entry.device_ext.device_settings.topic.is_some();
                let ble_only = undoc.entry.device_ext.device_settings.wifi_name.is_none();
                log::info!(
                    "  Undoc: room={room:?} supports_iot={supports_iot} ble_only={ble_only}"
                );
                log::trace!("{undoc:#?}");
            }
            if let Some(quirk) = device.resolve_quirk() {
                log::info!("  {quirk:?}");

                if quirk.lan_api_capable && device.lan_device.is_none() {
                    log::warn!(
                        "  This device should be available via the LAN API, \
                        but didn't respond to probing yet. Possible causes:"
                    );
                    log::warn!("  1) LAN API needs to be enabled in the Govee Home App.");
                    log::warn!("  2) The device is offline.");
                    log::warn!("  3) A network configuration issue is preventing communication.");
                    log::warn!(
                        "  4) The device needs a firmware update before it can enable LAN API."
                    );
                    log::warn!(
                        "  5) The hardware version of the device is too old to enable the LAN API."
                    );
                }
            } else if device.http_device_info.is_none() {
                log::warn!("  Unknown device type. Cannot map to Home Assistant.");
                if state.get_platform_client().await.is_none() {
                    log::warn!(
                        "  Recommendation: configure your Govee API Key so that \
                                  metadata can be fetched from Govee"
                    );
                }
            }

            log::info!("");
        }
    }

    /// Start all runtime services: extensions, polling, MQTT, HTTP.
    async fn start_services(
        http_port: u16,
        state: &StateHandle,
        args: &crate::Args,
    ) -> anyhow::Result<()> {
        let mut extensions = ExtensionManager::new();
        extensions.add(AvailabilityExtension::new());
        extensions.add(HealthExtension);
        extensions.add(ConfigReloadExtension);
        extensions.add(DiscoveryExtension::new());
        let extensions = Arc::new(extensions);
        extensions.start_all().await;

        {
            let state = state.clone();
            let extensions = extensions.clone();
            tokio::spawn(async move {
                if let Err(err) = periodic_state_poll(state, extensions).await {
                    log::error!("periodic_state_poll: {err:#}");
                }
            });
        }

        spawn_hass_integration(state.clone(), &args.hass_args).await?;

        if let Ok(Some(api_key)) = args.api_args.opt_api_key() {
            if let Err(err) =
                crate::service::govee_push::start_govee_push_client(&api_key, state.clone()).await
            {
                log::warn!("Failed to start Govee push client: {err:#}");
            }
        }

        let shutdown_state = state.clone();
        let shutdown_extensions = extensions.clone();

        tokio::select! {
            result = run_http_server(state.clone(), http_port) => {
                result.with_context(|| format!("Starting HTTP service on port {http_port}"))?;
            }
            _ = async {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sigterm = signal(SignalKind::terminate())
                        .expect("register SIGTERM handler");
                    let mut sigint = signal(SignalKind::interrupt())
                        .expect("register SIGINT handler");
                    tokio::select! {
                        _ = sigterm.recv() => log::info!("Received SIGTERM"),
                        _ = sigint.recv() => log::info!("Received SIGINT"),
                    }
                }
                #[cfg(not(unix))]
                {
                    tokio::signal::ctrl_c().await.expect("register ctrl-c handler");
                    log::info!("Received Ctrl-C");
                }
            } => {
                shutdown_extensions.stop_all(&shutdown_state).await;
                shutdown_state.graceful_shutdown().await;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_poll_interval_defaults_to_120s() {
        assert_eq!(parse_poll_interval(None), chrono::Duration::seconds(120));
    }

    #[test]
    fn parse_poll_interval_accepts_valid_override() {
        assert_eq!(
            parse_poll_interval(Some("900")),
            chrono::Duration::seconds(900)
        );
    }

    #[test]
    fn parse_poll_interval_ignores_non_numeric() {
        assert_eq!(
            parse_poll_interval(Some("not_a_number")),
            chrono::Duration::seconds(120)
        );
    }

    #[test]
    fn parse_poll_interval_ignores_empty_string() {
        assert_eq!(
            parse_poll_interval(Some("")),
            chrono::Duration::seconds(120)
        );
    }
}
