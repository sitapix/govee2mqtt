use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::service::device_config::DeviceGroup;
use crate::service::hass::{availability_topic, HassClient};
use crate::service::state::StateHandle;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// A light entity that controls multiple physical devices as one group.
/// Commands are sent to all members in parallel.
#[derive(Serialize, Clone, Debug)]
pub struct GroupLightConfig {
    #[serde(flatten)]
    pub base: EntityConfig,
    pub schema: String,
    pub command_topic: String,
    pub state_topic: String,
    pub supported_color_modes: Vec<String>,
    pub brightness: bool,
    pub brightness_scale: u32,
}

pub struct GroupLight {
    config: GroupLightConfig,
    member_ids: Vec<String>,
    state: StateHandle,
}

impl GroupLight {
    pub fn new(group_id: &str, group: &DeviceGroup, state: &StateHandle) -> Self {
        let safe_id = group_id.replace(' ', "_").to_ascii_lowercase();
        let command_topic = format!("gv2mqtt/group/{safe_id}/command");
        let state_topic = format!("gv2mqtt/group/{safe_id}/state");
        let unique_id = format!("gv2mqtt-group-{safe_id}");

        Self {
            config: GroupLightConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    availability: vec![],
                    availability_mode: None,
                    name: Some(group.name.clone()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device {
                        name: group.name.clone(),
                        manufacturer: "Govee".to_string(),
                        model: "Group".to_string(),
                        sw_version: None,
                        suggested_area: group.room.clone(),
                        via_device: Some("gv2mqtt".to_string()),
                        identifiers: vec![unique_id.clone()],
                        connections: vec![],
                    },
                    unique_id,
                    entity_category: None,
                    icon: group.icon.clone(),
                },
                schema: "json".to_string(),
                command_topic,
                state_topic,
                supported_color_modes: vec!["rgb".to_string(), "color_temp".to_string()],
                brightness: true,
                brightness_scale: 100,
            },
            member_ids: group.members.clone(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for GroupLight {
    async fn publish_config(
        &self,
        state: &StateHandle,
        client: &HassClient,
    ) -> anyhow::Result<()> {
        publish_entity_config("light", state, client, &self.config.base, &self.config).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        // Aggregate state from all members: use first member's state as representative.
        // If any member is ON, group is ON. Brightness = average. Color = first member.
        let mut any_on = false;
        let mut brightness_sum = 0u32;
        let mut brightness_count = 0u32;
        let mut color = None;
        let mut kelvin = 0u32;

        for member_id in &self.member_ids {
            if let Some(device) = self.state.device_by_id(member_id).await {
                if let Some(ds) = device.device_state() {
                    if ds.on {
                        any_on = true;
                    }
                    brightness_sum += ds.brightness as u32;
                    brightness_count += 1;
                    if color.is_none() {
                        color = Some(ds.color);
                        kelvin = ds.kelvin;
                    }
                }
            }
        }

        let state_str = if any_on { "ON" } else { "OFF" };
        let avg_brightness = brightness_sum.checked_div(brightness_count).unwrap_or(0);

        let mut payload = Map::new();
        payload.insert("state".to_string(), json!(state_str));
        if any_on {
            payload.insert("brightness".to_string(), json!(avg_brightness));
            if let Some(c) = color {
                if kelvin == 0 {
                    payload.insert("color_mode".to_string(), json!("rgb"));
                    payload.insert(
                        "color".to_string(),
                        json!({"r": c.r, "g": c.g, "b": c.b}),
                    );
                } else {
                    payload.insert("color_mode".to_string(), json!("color_temp"));
                }
            }
        }

        client
            .publish(&self.config.state_topic, &Value::Object(payload).to_string())
            .await
    }
}
