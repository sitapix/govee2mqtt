use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::platform_api::DeviceCapability;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    availability_topic, camel_case_to_space_separated, device_availability_entries, topic_safe_id,
    topic_safe_string, HassClient,
};
use crate::service::state::StateHandle;
use async_trait::async_trait;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct ButtonConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub command_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_press: Option<String>,
}

impl ButtonConfig {
    #[allow(dead_code)]
    pub async fn for_device(
        device: &ServiceDevice,
        instance: &DeviceCapability,
    ) -> anyhow::Result<Self> {
        let command_topic = format!(
            "gv2mqtt/switch/{id}/command/{inst}",
            id = topic_safe_id(device),
            inst = instance.instance
        );
        let unique_id = format!(
            "gv2mqtt-{id}-{inst}",
            id = topic_safe_id(device),
            inst = instance.instance
        );

        Ok(Self {
            base: EntityConfig::for_device(
                device,
                Some(camel_case_to_space_separated(&instance.instance)),
                unique_id,
            ),
            command_topic,
            payload_press: None,
        })
    }

    pub fn new<NAME: Into<String>, TOPIC: Into<String>>(name: NAME, topic: TOPIC) -> Self {
        let name = name.into();
        let unique_id = format!("global-{}", topic_safe_string(&name));
        Self {
            base: EntityConfig {
                availability_topic: availability_topic(),
                availability: vec![],
                availability_mode: None,
                name: Some(name.to_string()),
                entity_category: None,
                origin: Origin::default(),
                device: Device::this_service(),
                unique_id: unique_id.clone(),
                device_class: None,
                icon: None,
            },
            command_topic: topic.into(),
            payload_press: None,
        }
    }

    pub fn activate_work_mode_preset(
        device: &ServiceDevice,
        name: &str,
        mode_name: &str,
        mode_num: i64,
        value: i64,
    ) -> Self {
        let unique_id = format!(
            "gv2mqtt-{id}-preset-{mode}-{mode_num}-{value}",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name),
        );
        let command_topic = format!(
            "gv2mqtt/number/{id}/command/{mode}/{mode_num}",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name),
        );
        Self {
            base: EntityConfig::for_device(device, Some(name.to_string()), unique_id.clone()),
            command_topic,
            payload_press: Some(value.to_string()),
        }
    }

    pub fn request_platform_data_for_device(device: &ServiceDevice) -> Self {
        let unique_id = format!(
            "gv2mqtt-{id}-request-platform-data",
            id = topic_safe_id(device)
        );
        let command_topic = format!(
            "gv2mqtt/{id}/request-platform-data",
            id = topic_safe_id(device)
        );
        let (availability, availability_mode) = device_availability_entries(device);
        Self {
            base: EntityConfig {
                availability_topic: String::new(),
                availability,
                availability_mode,
                name: Some("Request Platform API State".to_string()),
                entity_category: Some("diagnostic".to_string()),
                origin: Origin::default(),
                device: Device::for_device(device),
                unique_id: unique_id.clone(),
                device_class: None,
                icon: None,
            },
            command_topic,
            payload_press: None,
        }
    }
}

#[async_trait]
impl EntityInstance for ButtonConfig {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("button", state, client, &self.base, self).await
    }

    async fn notify_state(&self, _client: &HassClient) -> anyhow::Result<()> {
        // Buttons have no state
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ButtonConfig;
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::service::device::Device;

    #[test]
    fn global_button_has_expected_unique_id_and_topic() {
        let button = ButtonConfig::new("Restart Bridge", "gv2mqtt/bridge/restart");
        assert_eq!(button.base.name.as_deref(), Some("Restart Bridge"));
        assert_eq!(button.command_topic, "gv2mqtt/bridge/restart");
        assert_eq!(button.base.unique_id, "global-restart_bridge");
        assert!(button.payload_press.is_none());
    }

    #[test]
    fn activate_work_mode_preset_has_expected_fields() {
        let device = Device::new("H7160", "AA:BB");
        let button = ButtonConfig::activate_work_mode_preset(&device, "High", "humidity", 1, 8);
        assert_eq!(button.base.name.as_deref(), Some("High"));
        assert_eq!(button.command_topic, "gv2mqtt/number/AABB/command/humidity/1");
        assert_eq!(button.payload_press.as_deref(), Some("8"));
    }

    #[test]
    fn request_platform_data_button_has_diagnostic_category() {
        let device = Device::new("H6000", "AA:BB");
        let button = ButtonConfig::request_platform_data_for_device(&device);
        assert_eq!(
            button.base.name.as_deref(),
            Some("Request Platform API State")
        );
        assert_eq!(
            button.base.entity_category.as_deref(),
            Some("diagnostic")
        );
        assert_eq!(button.command_topic, "gv2mqtt/AABB/request-platform-data");

        let _entity_trait: &dyn EntityInstance = &button;
    }
}
