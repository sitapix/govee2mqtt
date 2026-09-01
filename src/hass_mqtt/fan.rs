//! Home Assistant MQTT Fan entity for Govee fan devices (H7105 and friends).
//!
//! The protocol wiring is intentionally thin: we reuse the existing MQTT
//! routes that already exist for humidifier/work-mode control, so the Fan
//! entity is essentially a second facade over the same `workMode` machinery.
//!
//! - power: publishes to `gv2mqtt/switch/{id}/command/powerSwitch`
//! - preset modes: publishes to `gv2mqtt/{id}/set-work-mode` (handled by
//!   `humidifier::mqtt_device_set_work_mode`)
//! - speed percentage: publishes to `gv2mqtt/number/{id}/command/FanSpeed/{n}`
//!   (handled by `number::mqtt_number_command`)
//!
//! See HA docs: <https://www.home-assistant.io/integrations/fan.mqtt>

use crate::hass_mqtt::base::EntityConfig;
use crate::hass_mqtt::instance::{lookup_entity_device, publish_entity_config, EntityInstance};
use crate::hass_mqtt::work_mode::ParsedWorkMode;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{topic_safe_id, HassClient};
use crate::service::state::StateHandle;
use async_trait::async_trait;
use serde::Serialize;

const FAN_SPEED_MODE: &str = "FanSpeed";

/// HA MQTT Fan config. Only the fields we actually use are typed here; HA
/// silently ignores unknown JSON keys.
#[derive(Serialize, Clone, Debug)]
pub struct FanConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub command_topic: String,
    pub state_topic: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage_command_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage_state_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_range_min: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_range_max: Option<u8>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_mode_command_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_mode_state_topic: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub preset_modes: Vec<String>,

    pub optimistic: bool,
}

#[derive(Clone)]
pub struct Fan {
    fan: FanConfig,
    state: StateHandle,
    device_id: String,
}

impl Fan {
    pub async fn new(device: &ServiceDevice, state: &StateHandle) -> anyhow::Result<Self> {
        let use_iot = device.iot_api_supported() && state.get_iot_client().await.is_some();
        let optimistic = !use_iot;

        let id = topic_safe_id(device);
        let command_topic = format!("gv2mqtt/switch/{id}/command/powerSwitch");
        let state_topic = format!("gv2mqtt/fan/{id}/state");

        let mut preset_modes: Vec<String> = vec![];
        let mut fan_speed_mode_value: Option<i64> = None;
        let mut speed_range_min: Option<u8> = None;
        let mut speed_range_max: Option<u8> = None;

        if let Ok(wm) = ParsedWorkMode::with_device(device) {
            for name in wm.get_mode_names() {
                let Some(mode) = wm.mode_by_name(&name) else {
                    continue;
                };
                if name.eq_ignore_ascii_case(FAN_SPEED_MODE) {
                    if let Some(range) = mode.contiguous_value_range() {
                        speed_range_min = Some(range.start.clamp(1, 255) as u8);
                        let end_inclusive = (range.end - 1).max(range.start);
                        speed_range_max = Some(end_inclusive.clamp(1, 255) as u8);
                        fan_speed_mode_value = mode.value.as_i64();
                        if fan_speed_mode_value.is_none() {
                            log::debug!(
                                "{device}: FanSpeed work mode value is not an integer ({:?}); \
                                 HA percentage control will be disabled",
                                mode.value
                            );
                        }
                    }
                } else if mode.should_show_as_preset() {
                    preset_modes.push(name);
                }
            }
        }

        let percentage_command_topic = fan_speed_mode_value
            .map(|mode_val| format!("gv2mqtt/number/{id}/command/FanSpeed/{mode_val}"));
        let percentage_state_topic = fan_speed_mode_value
            .as_ref()
            .map(|_| format!("gv2mqtt/fan/{id}/percentage"));

        let preset_mode_command_topic =
            (!preset_modes.is_empty()).then(|| format!("gv2mqtt/{id}/set-work-mode"));
        let preset_mode_state_topic =
            (!preset_modes.is_empty()).then(|| format!("gv2mqtt/fan/{id}/preset-mode"));

        let unique_id = format!("gv2mqtt-{id}-fan");

        Ok(Self {
            fan: FanConfig {
                base: EntityConfig::for_device(device, None, unique_id),
                command_topic,
                state_topic,
                percentage_command_topic,
                percentage_state_topic,
                speed_range_min,
                speed_range_max,
                preset_mode_command_topic,
                preset_mode_state_topic,
                preset_modes,
                optimistic,
            },
            state: state.clone(),
            device_id: device.id.to_string(),
        })
    }
}

#[async_trait]
impl EntityInstance for Fan {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("fan", state, client, &self.fan.base, &self.fan).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) = lookup_entity_device(&self.state, &self.device_id, "fan entity").await
        else {
            return Ok(());
        };

        let is_on = device.device_state().map(|s| s.on).unwrap_or(false);
        client
            .publish(&self.fan.state_topic, if is_on { "ON" } else { "OFF" })
            .await?;

        let Some(cap) = device.get_state_capability_by_instance("workMode") else {
            return Ok(());
        };
        let Some(mode_num) = cap.state.pointer("/value/workMode") else {
            return Ok(());
        };
        let Ok(work_modes) = ParsedWorkMode::with_device(&device) else {
            return Ok(());
        };
        let Some(active) = work_modes.mode_for_value(mode_num) else {
            return Ok(());
        };

        if active.name.eq_ignore_ascii_case(FAN_SPEED_MODE) {
            if let (Some(topic), Some(v)) = (
                self.fan.percentage_state_topic.as_ref(),
                cap.state
                    .pointer("/value/modeValue")
                    .and_then(|v| v.as_i64()),
            ) {
                client.publish(topic, v.to_string()).await?;
            }
        } else if let Some(topic) = &self.fan.preset_mode_state_topic {
            client.publish(topic, active.name.to_string()).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Fan;
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::platform_api::{DeviceCapability, DeviceType, HttpDeviceInfo};
    use crate::service::device::Device as ServiceDevice;
    use crate::service::hass::HassClient;
    use crate::service::state::State;
    use std::sync::Arc;

    fn fan_device_info(capabilities: Vec<DeviceCapability>) -> HttpDeviceInfo {
        HttpDeviceInfo {
            sku: "H7105".to_string(),
            device: "AA:BB".to_string(),
            device_name: "Tower Fan".to_string(),
            device_type: DeviceType::Fan,
            capabilities,
        }
    }

    fn work_mode_capability() -> DeviceCapability {
        serde_json::from_str(
            r#"{
                "type": "devices.capabilities.work_mode",
                "instance": "workMode",
                "parameters": {
                    "dataType": "STRUCT",
                    "fields": [
                        {
                            "fieldName": "workMode",
                            "dataType": "ENUM",
                            "options": [
                                {"name": "FanSpeed", "value": 1},
                                {"name": "Auto", "value": 3},
                                {"name": "Sleep", "value": 5}
                            ],
                            "required": true
                        },
                        {
                            "fieldName": "modeValue",
                            "dataType": "ENUM",
                            "options": [
                                {
                                    "name": "FanSpeed",
                                    "value": 0,
                                    "range": {"min": 1, "max": 8, "precision": 1}
                                }
                            ],
                            "required": false
                        }
                    ]
                }
            }"#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn fan_publishes_percentage_and_preset_topics_when_work_modes_present() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;

        let mut device = ServiceDevice::new("H7105", "AA:BB");
        device.http_device_info = Some(fan_device_info(vec![work_mode_capability()]));

        let fan = Fan::new(&device, &state).await.unwrap();

        assert_eq!(fan.fan.speed_range_min, Some(1));
        assert_eq!(fan.fan.speed_range_max, Some(8));
        assert!(fan.fan.percentage_command_topic.is_some());
        assert!(fan.fan.preset_mode_command_topic.is_some());
        assert!(fan
            .fan
            .preset_modes
            .iter()
            .any(|m| m == "Auto" || m == "Sleep"));

        let client = HassClient::new_test();
        fan.publish_config(&state, &client).await.unwrap();
        let published = client.published_messages();
        assert!(!published.is_empty());
        assert!(published[0].0.contains("/fan/"));
        assert!(published[0].1.contains("\"speed_range_min\":1"));
        assert!(published[0].1.contains("\"speed_range_max\":8"));
    }

    #[tokio::test]
    async fn fan_works_without_work_modes() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;

        let mut device = ServiceDevice::new("H7105", "AA:BB");
        device.http_device_info = Some(fan_device_info(vec![]));

        let fan = Fan::new(&device, &state).await.unwrap();
        assert!(fan.fan.percentage_command_topic.is_none());
        assert!(fan.fan.preset_mode_command_topic.is_none());
        assert!(fan.fan.preset_modes.is_empty());
    }
}
