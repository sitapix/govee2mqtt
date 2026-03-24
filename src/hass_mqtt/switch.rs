use crate::hass_mqtt::base::EntityConfig;
use crate::hass_mqtt::instance::{lookup_entity_device, publish_entity_config, EntityInstance};
use crate::platform_api::DeviceCapability;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    camel_case_to_space_separated, switch_instance_state_topic,
    topic_safe_id, HassClient, IdParameter,
};
use crate::service::state::StateHandle;
use anyhow::Context;
use async_trait::async_trait;
use mosquitto_rs::router::{Params, Payload, State};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize, Clone, Debug)]
pub struct SwitchConfig {
    #[serde(flatten)]
    pub base: EntityConfig,
    pub command_topic: String,
    pub state_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_by_default: Option<bool>,
}

impl SwitchConfig {
    pub async fn for_device(
        device: &ServiceDevice,
        instance: &DeviceCapability,
    ) -> anyhow::Result<Self> {
        let command_topic = format!(
            "gv2mqtt/switch/{id}/command/{inst}",
            id = topic_safe_id(device),
            inst = instance.instance
        );
        let state_topic = switch_instance_state_topic(device, &instance.instance);
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
            state_topic,
            enabled_by_default: None,
        })
    }

    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("switch", state, client, &self.base, self).await
    }
}

pub struct CapabilitySwitch {
    switch: SwitchConfig,
    device_id: String,
    state: StateHandle,
    instance_name: String,
}

impl CapabilitySwitch {
    pub async fn new(
        device: &ServiceDevice,
        state: &StateHandle,
        instance: &DeviceCapability,
    ) -> anyhow::Result<Self> {
        let switch = SwitchConfig::for_device(device, instance).await?;
        Ok(Self {
            switch,
            device_id: device.id.to_string(),
            state: state.clone(),
            instance_name: instance.instance.to_string(),
        })
    }
}

#[async_trait]
impl EntityInstance for CapabilitySwitch {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.switch.publish(&state, &client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "capability switch").await
        else {
            return Ok(());
        };

        if self.instance_name == "powerSwitch" {
            if let Some(state) = device.device_state() {
                client
                    .publish(
                        &self.switch.state_topic,
                        if state.on { "ON" } else { "OFF" },
                    )
                    .await?;
            }
            return Ok(());
        }

        // TODO: currently, Govee don't return any meaningful data on
        // additional states. When they do, we'll need to start reporting
        // it here, but we'll also need to start polling it from the
        // platform API in order for it to even be available here.
        // Until then, the switch will show in the hass UI with an
        // unknown state but provide you with separate on and off push
        // buttons so that you can at least send the commands to the device.
        // <https://developer.govee.com/discuss/6596e84c901fb900312d5968>

        if let Some(cap) = device.get_state_capability_by_instance(&self.instance_name) {
            match cap.state.pointer("/value").and_then(|v| v.as_i64()) {
                Some(n) => {
                    return client
                        .publish(&self.switch.state_topic, if n != 0 { "ON" } else { "OFF" })
                        .await;
                }
                None => {
                    if cap.state.pointer("/value") == Some(&json!("")) {
                        log::trace!(
                            "CapabilitySwitch::notify_state ignore useless \
                                            empty string state for {cap:?}"
                        );
                    } else {
                        log::warn!("CapabilitySwitch::notify_state: Do something with {cap:#?}");
                    }
                    return Ok(());
                }
            }
        }
        log::trace!(
            "CapabilitySwitch::notify_state: didn't find state for {device} {instance}",
            instance = self.instance_name
        );
        Ok(())
    }
}

pub struct MusicAutoColorSwitch {
    switch: SwitchConfig,
    device_id: String,
    state: StateHandle,
}

impl MusicAutoColorSwitch {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        let unique_id = format!("gv2mqtt-{id}-music-auto-color", id = topic_safe_id(device));

        Self {
            switch: SwitchConfig {
                base: EntityConfig::for_device(
                    device,
                    Some("Music Auto Color".to_string()),
                    unique_id,
                ),
                command_topic: format!(
                    "gv2mqtt/{id}/set-music-auto-color",
                    id = topic_safe_id(device)
                ),
                state_topic: format!(
                    "gv2mqtt/switch/{id}/music-auto-color/state",
                    id = topic_safe_id(device)
                ),
                enabled_by_default: Some(false),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for MusicAutoColorSwitch {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.switch.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "music auto color switch").await
        else {
            return Ok(());
        };

        let enabled = if let Some(cap) = device.get_state_capability_by_instance("musicMode") {
            cap.state
                .pointer("/value/autoColor")
                .and_then(|value| value.as_i64())
                .map(|value| value != 0)
        } else {
            device.active_music_mode().map(|music| music.auto_color)
        };

        if let Some(enabled) = enabled {
            client
                .publish(&self.switch.state_topic, if enabled { "ON" } else { "OFF" })
                .await?;
        }

        Ok(())
    }
}

pub async fn mqtt_set_music_auto_color(
    Payload(command): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;
    let auto_color = match command.as_str() {
        "ON" | "on" => true,
        "OFF" | "off" => false,
        _ => anyhow::bail!("invalid music auto color state {command}"),
    };

    state
        .device_set_music_auto_color(&device, auto_color)
        .await
        .context("mqtt_set_music_auto_color: state.device_set_music_auto_color")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::MusicAutoColorSwitch;
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::service::device::Device;
    use crate::service::state::State;
    use std::sync::Arc;

    #[tokio::test]
    async fn switch_for_device_uses_entity_config_for_device() {
        use crate::hass_mqtt::switch::SwitchConfig;
        use crate::platform_api::{DeviceCapability, DeviceCapabilityKind};

        let device = Device::new("H6000", "AA:BB");
        let cap = DeviceCapability {
            kind: DeviceCapabilityKind::Toggle,
            instance: "powerSwitch".to_string(),
            parameters: None,
            alarm_type: None,
            event_state: None,
        };
        let switch = SwitchConfig::for_device(&device, &cap).await.unwrap();
        assert_eq!(
            switch.base.name.as_deref(),
            Some("Power Switch")
        );
        assert_eq!(switch.command_topic, "gv2mqtt/switch/AABB/command/powerSwitch");
        assert_eq!(
            switch.state_topic,
            "gv2mqtt/switch/AABB/powerSwitch/state"
        );
        assert!(switch.enabled_by_default.is_none());
    }

    #[test]
    fn music_auto_color_switch_has_expected_topics_and_registry_defaults() {
        let device = Device::new("H6000", "AA:BB");
        let state = Arc::new(State::new());
        let entity = MusicAutoColorSwitch::new(&device, &state);

        assert_eq!(entity.switch.base.name.as_deref(), Some("Music Auto Color"));
        assert_eq!(
            entity.switch.command_topic,
            "gv2mqtt/AABB/set-music-auto-color"
        );
        assert_eq!(
            entity.switch.state_topic,
            "gv2mqtt/switch/AABB/music-auto-color/state"
        );
        assert_eq!(entity.switch.enabled_by_default, Some(false));

        let _entity_trait: &dyn EntityInstance = &entity;
    }
}
