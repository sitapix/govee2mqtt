//! Official Govee MQTT push API client.
//!
//! Connects to mqtt.openapi.govee.com:8883 using the Platform API key
//! and subscribes to `GA/{api_key}` for real-time device state events.
//!
//! This provides push-based state updates without polling, complementing
//! the LAN and IoT (undocumented) channels.
//!
//! Discovered via bigboxer23/govee-java-api.

use crate::platform_api::from_json;
use crate::service::state::StateHandle;
use anyhow::Context;
use mosquitto_rs::{Client, Event, QoS};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::time::Duration;

const GOVEE_MQTT_HOST: &str = "mqtt.openapi.govee.com";
const GOVEE_MQTT_PORT: u16 = 8883;

/// A push event received from Govee's official MQTT API.
#[derive(Deserialize, Debug)]
pub struct GoveeEvent {
    pub sku: Option<String>,
    pub device: Option<String>,
    #[serde(rename = "deviceName")]
    pub device_name: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<GoveeEventCapability>,
}

#[derive(Deserialize, Debug)]
pub struct GoveeEventCapability {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub instance: Option<String>,
    pub state: Option<JsonValue>,
}

impl GoveeEvent {
    pub fn is_lack_water_event(&self) -> bool {
        self.capabilities
            .first()
            .and_then(|cap| cap.instance.as_deref())
            .map(|inst| inst.eq_ignore_ascii_case("lackWaterEvent"))
            .unwrap_or(false)
    }
}

/// Start the Govee MQTT push client. Connects to the official API
/// and processes incoming device state events.
pub async fn start_govee_push_client(api_key: &str, state: StateHandle) -> anyhow::Result<()> {
    let client_id = format!("govee2mqtt-push-{}", uuid::Uuid::new_v4().simple());
    let client = Client::with_id(&client_id, true)?;

    client.set_username_and_password(Some(api_key), Some(api_key))?;

    // TLS without client certs (server cert only)
    // Find a system CA bundle for TLS verification
    let ca_paths = [
        "/etc/ssl/certs",                             // Debian/Ubuntu
        "/etc/pki/tls/certs",                         // RHEL/CentOS
        "/usr/local/share/certs",                     // FreeBSD
        "/etc/ssl",                                   // Alpine
        "/opt/homebrew/etc/ca-certificates/cert.pem", // macOS Homebrew
    ];
    let ca_path = ca_paths.iter().find(|p| std::path::Path::new(p).exists());

    if let Some(ca) = ca_path {
        client
            .configure_tls(
                None::<&std::path::Path>,
                Some(std::path::Path::new(ca)),
                None::<&std::path::Path>,
                None::<&std::path::Path>,
                None,
            )
            .context("configure TLS for Govee push")?;
    } else {
        log::warn!("No system CA bundle found for Govee push TLS. Trying without CA path.");
        client
            .configure_tls(
                None::<&std::path::Path>,
                None::<&std::path::Path>,
                None::<&std::path::Path>,
                None::<&std::path::Path>,
                None,
            )
            .context("configure TLS for Govee push")?;
    }

    log::info!("Connecting to Govee push API at {GOVEE_MQTT_HOST}:{GOVEE_MQTT_PORT}...");

    match tokio::time::timeout(
        Duration::from_secs(30),
        client.connect(
            GOVEE_MQTT_HOST,
            GOVEE_MQTT_PORT.into(),
            Duration::from_secs(120),
            None,
        ),
    )
    .await
    {
        Ok(Ok(status)) => {
            log::info!("Connected to Govee push API: {status}");
        }
        Ok(Err(err)) => {
            log::warn!("Failed to connect to Govee push API: {err:#}. Push updates disabled.");
            return Ok(());
        }
        Err(_) => {
            log::warn!("Timeout connecting to Govee push API. Push updates disabled.");
            return Ok(());
        }
    }

    let topic = format!("GA/{api_key}");
    let subscriber = client.subscriber().expect("own the subscriber");

    tokio::spawn(async move {
        if let Err(err) = run_push_loop(subscriber, state, client, topic).await {
            log::error!("Govee push loop failed: {err:#}");
        }
        log::info!("Govee push loop terminated");
    });

    Ok(())
}

async fn run_push_loop(
    subscriber: async_channel::Receiver<Event>,
    state: StateHandle,
    client: Client,
    topic: String,
) -> anyhow::Result<()> {
    while let Ok(event) = subscriber.recv().await {
        match event {
            Event::Connected(status) => {
                log::info!("Govee push (re)connected: {status}");
                state
                    .push_connected
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                if let Err(err) = client.subscribe(&topic, QoS::AtLeastOnce).await {
                    log::error!("Failed to subscribe to Govee push topic: {err:#}");
                } else {
                    log::info!("Subscribed to Govee push topic: {topic}");
                }
            }
            Event::Disconnected(reason) => {
                log::warn!("Govee push disconnected: {reason}. Will auto-reconnect.");
                state
                    .push_connected
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
            Event::Message(msg) => {
                let payload = String::from_utf8_lossy(&msg.payload);
                log::debug!("Govee push event: {payload}");

                match from_json::<GoveeEvent, _>(&msg.payload) {
                    Ok(event) => {
                        process_push_event(&state, &event).await;
                    }
                    Err(err) => {
                        log::debug!("Failed to parse Govee push event: {err:#}");
                    }
                }
            }
        }
    }

    Ok(())
}

async fn process_push_event(state: &StateHandle, event: &GoveeEvent) {
    let Some(device_id) = &event.device else {
        return;
    };
    let Some(sku) = &event.sku else {
        return;
    };

    state
        .push_event_count
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    log::info!(
        "Govee push: state update for {} ({})",
        event.device_name.as_deref().unwrap_or(device_id),
        sku
    );

    // Publish the raw push event to MQTT for user visibility
    if let Some(hass) = state.get_hass_client().await {
        let safe_id = crate::service::hass::topic_safe_id_str(device_id);
        let topic = format!("gv2mqtt/{safe_id}/push_event");
        let payload = serde_json::json!({
            "device": device_id,
            "sku": sku,
            "name": event.device_name,
            "capabilities": event.capabilities.iter().map(|c| {
                serde_json::json!({
                    "type": c.kind,
                    "instance": c.instance,
                    "state": c.state,
                })
            }).collect::<Vec<_>>(),
        });
        let _ = hass.publish(&topic, payload.to_string()).await;
    }

    // Check for special events
    if event.is_lack_water_event() {
        log::warn!("Govee push: lack water event for {} ({})", device_id, sku);

        // Publish to MQTT so HA automations can react
        if let Some(hass) = state.get_hass_client().await {
            let topic = format!(
                "gv2mqtt/{}/lack_water",
                crate::service::hass::topic_safe_id_str(device_id)
            );
            let _ = hass.publish(&topic, "true").await;
        }
    }

    // Update device state from capabilities
    {
        let device = state.device_by_id(device_id).await;
        if device.is_none() {
            // Device not yet known — create it
            let mut device = state.device_mut(sku, device_id).await;
            if device.http_device_info.is_none() {
                if let Some(name) = &event.device_name {
                    device.set_http_device_info(crate::platform_api::HttpDeviceInfo {
                        sku: sku.clone(),
                        device: device_id.clone(),
                        device_name: name.clone(),
                        device_type: Default::default(),
                        capabilities: vec![],
                    });
                }
            }
        }
    }

    // Update capability states on the device
    if !event.capabilities.is_empty() {
        let mut device = state.device_mut(sku, device_id).await;
        if let Some(http_state) = &mut device.http_device_state {
            for cap in &event.capabilities {
                if let (Some(instance), Some(state_val)) = (&cap.instance, &cap.state) {
                    // Update matching capability state
                    for existing in &mut http_state.capabilities {
                        if existing.instance.eq_ignore_ascii_case(instance) {
                            existing.state = state_val.clone();
                        }
                    }
                }
            }
        }
        device.set_last_polled();
    }

    // Notify HA of the state change
    if let Err(err) = state.notify_of_state_change(device_id).await {
        log::debug!("Failed to notify state change from push event: {err:#}");
    }
}
