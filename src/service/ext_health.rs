use crate::service::extension::Extension;
use crate::service::state::StateHandle;
use async_trait::async_trait;

/// Publishes bridge health and device list to MQTT periodically.
/// Topics: gv2mqtt/bridge/health, gv2mqtt/bridge/devices
pub struct HealthExtension;

#[async_trait]
impl Extension for HealthExtension {
    fn name(&self) -> &str {
        "health"
    }

    async fn tick(&self, state: &StateHandle) -> anyhow::Result<()> {
        publish_bridge_health(state).await;
        publish_bridge_devices(state).await;
        Ok(())
    }
}

pub async fn publish_bridge_health(state: &StateHandle) {
    let Some(hass) = state.get_hass_client().await else {
        return;
    };

    let devices = state.devices().await;
    let now = chrono::Utc::now();

    let mut online_count = 0u32;
    let mut offline_count = 0u32;
    let mut lan_count = 0u32;
    let mut iot_count = 0u32;

    for device in &devices {
        let is_online = device.is_online(now);

        if is_online {
            online_count += 1;
        } else {
            offline_count += 1;
        }
        if device.lan_device.is_some() {
            lan_count += 1;
        }
        if device.undoc_device_info.is_some() {
            iot_count += 1;
        }
    }

    let health = serde_json::json!({
        "version": crate::version_info::govee_version(),
        "devices": {
            "total": devices.len(),
            "online": online_count,
            "offline": offline_count,
            "lan": lan_count,
            "iot": iot_count,
        },
        "apis": {
            "platform": state.get_platform_client().await.is_some(),
            "undoc": state.get_undoc_client().await.is_some(),
            "lan": state.get_lan_client().await.is_some(),
            "iot": state.get_iot_client().await.is_some(),
            "push": {
                "connected": state.push_connected.load(std::sync::atomic::Ordering::Relaxed),
                "events_received": state.push_event_count.load(std::sync::atomic::Ordering::Relaxed),
            },
        },
        "timestamp": now.to_rfc3339(),
    });

    if let Err(err) = hass
        .publish_retained("gv2mqtt/bridge/health", health.to_string())
        .await
    {
        log::warn!("Failed to publish bridge health: {err:#}");
    }
}

pub async fn publish_bridge_devices(state: &StateHandle) {
    let Some(hass) = state.get_hass_client().await else {
        return;
    };

    let devices = state.devices().await;
    let now = chrono::Utc::now();

    let device_list: Vec<serde_json::Value> = devices
        .iter()
        .map(|d| {
            let is_online = d.is_online(now);

            serde_json::json!({
                "id": d.id,
                "sku": d.sku,
                "name": d.name(),
                "room": d.room_name(),
                "type": format!("{:?}", d.device_type()),
                "available": is_online,
                "apis": {
                    "lan": d.lan_device.is_some(),
                    "iot": d.undoc_device_info.is_some(),
                    "platform": d.http_device_info.is_some(),
                },
            })
        })
        .collect();

    let payload = serde_json::to_string(&device_list).unwrap_or_default();
    if let Err(err) = hass
        .publish_retained("gv2mqtt/bridge/devices", payload)
        .await
    {
        log::warn!("Failed to publish bridge devices: {err:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lan_api::{DeviceColor, DeviceStatus};
    use crate::service::hass::HassClient;
    use crate::service::state::State;
    use std::sync::Arc;

    #[tokio::test]
    async fn publish_bridge_health_with_empty_state_produces_valid_json() {
        let state: StateHandle = Arc::new(State::new());
        let client = HassClient::new_test();
        state.set_hass_client(client.clone()).await;

        publish_bridge_health(&state).await;

        let published = client.published_messages();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, "gv2mqtt/bridge/health");

        // Verify the payload is valid JSON
        let payload: serde_json::Value =
            serde_json::from_str(&published[0].1).unwrap();
        assert_eq!(payload["devices"]["total"], 0);
        assert_eq!(payload["devices"]["online"], 0);
        assert_eq!(payload["devices"]["offline"], 0);
        assert!(payload["version"].is_string());
        assert!(payload["timestamp"].is_string());
    }

    #[tokio::test]
    async fn publish_bridge_devices_with_devices_produces_correct_list() {
        let state: StateHandle = Arc::new(State::new());
        let client = HassClient::new_test();
        state.set_hass_client(client.clone()).await;

        // Add two devices with LAN status so they appear in the device list
        {
            let mut dev = state.device_mut("H6001", "AA:BB").await;
            dev.set_lan_device_status(DeviceStatus {
                on: true,
                brightness: 100,
                color: DeviceColor { r: 255, g: 0, b: 0 },
                color_temperature_kelvin: 0,
            });
        }
        {
            let mut dev = state.device_mut("H7012", "CC:DD").await;
            dev.set_lan_device_status(DeviceStatus {
                on: false,
                brightness: 0,
                color: DeviceColor { r: 0, g: 0, b: 0 },
                color_temperature_kelvin: 0,
            });
        }

        publish_bridge_devices(&state).await;

        let published = client.published_messages();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, "gv2mqtt/bridge/devices");

        let devices: Vec<serde_json::Value> =
            serde_json::from_str(&published[0].1).unwrap();
        assert_eq!(devices.len(), 2);

        // Check that device IDs are present (order may vary)
        let ids: Vec<&str> = devices.iter().map(|d| d["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"AA:BB"));
        assert!(ids.contains(&"CC:DD"));

        // Check structure of one device entry
        let dev = devices.iter().find(|d| d["id"] == "AA:BB").unwrap();
        assert_eq!(dev["sku"], "H6001");
        assert!(dev["apis"].is_object());
    }

    #[tokio::test]
    async fn publish_bridge_health_without_hass_client_is_noop() {
        let state: StateHandle = Arc::new(State::new());
        // No hass client set — should not panic
        publish_bridge_health(&state).await;
    }
}
