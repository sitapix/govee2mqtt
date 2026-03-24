use crate::service::extension::Extension;
use crate::service::state::StateHandle;
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::Mutex;

/// Tracks per-device availability and publishes online/offline to MQTT
/// with exponential backoff for offline devices.
pub struct AvailabilityExtension {
    /// Maps device_id -> (was_online, consecutive_offline_count)
    last_state: Mutex<HashMap<String, (bool, u32)>>,
}

impl AvailabilityExtension {
    pub fn new() -> Self {
        Self {
            last_state: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl Extension for AvailabilityExtension {
    fn name(&self) -> &str {
        "availability"
    }

    async fn tick(&self, state: &StateHandle) -> anyhow::Result<()> {
        let Some(hass) = state.get_hass_client().await else {
            return Ok(());
        };

        let devices = state.devices().await;
        let now = chrono::Utc::now();
        let mut last_avail = self.last_state.lock().await;

        for device in &devices {
            if device.is_ble_only_device() == Some(true) {
                continue;
            }

            let is_online = device.is_online(now);

            let entry = last_avail.entry(device.id.clone()).or_insert((false, 0));
            let (was_online, offline_count) = entry;

            let should_publish = if is_online {
                *offline_count = 0;
                is_online != *was_online || !*was_online
            } else {
                let interval = 1u32 << (*offline_count).min(5);
                *offline_count += 1;
                !*was_online && is_online != *was_online
                    || *offline_count % interval == 0
            };

            *was_online = is_online;

            if should_publish {
                let avail_topic = crate::service::hass::device_availability_topic(device);
                let status = if is_online { "online" } else { "offline" };
                if let Err(err) = hass.publish_retained(&avail_topic, status).await {
                    log::warn!("Failed to publish availability for {device}: {err:#}");
                }
            }
        }

        Ok(())
    }

    async fn stop(&self, state: &StateHandle) -> anyhow::Result<()> {
        let Some(hass) = state.get_hass_client().await else {
            return Ok(());
        };

        let devices = state.devices().await;
        for device in &devices {
            let avail_topic = crate::service::hass::device_availability_topic(device);
            let _ = hass.publish_retained(&avail_topic, "offline").await;
        }

        Ok(())
    }
}
