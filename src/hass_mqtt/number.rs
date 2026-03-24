use crate::hass_mqtt::base::EntityConfig;
use crate::hass_mqtt::instance::{lookup_entity_device, publish_entity_config, EntityInstance};
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    topic_safe_id, topic_safe_string, HassClient, IdParameter,
};
use crate::service::state::StateHandle;
use anyhow::anyhow;
use async_trait::async_trait;
use mosquitto_rs::router::{Params, Payload, State};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::ops::Range;

#[derive(Serialize, Clone, Debug)]
pub struct NumberConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub command_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
    pub step: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_of_measurement: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_by_default: Option<bool>,
}

impl NumberConfig {
    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("number", state, client, &self.base, self).await
    }

    pub async fn notify_state(&self, client: &HassClient, value: &str) -> anyhow::Result<()> {
        client
            .publish(
                self.state_topic
                    .as_deref()
                    .ok_or_else(|| anyhow!("number has no state_topic"))?,
                value,
            )
            .await
    }
}

pub struct WorkModeNumber {
    number: NumberConfig,
    device_id: String,
    state: StateHandle,
    mode_name: String,
    work_mode: JsonValue,
}

impl WorkModeNumber {
    pub fn new(
        device: &ServiceDevice,
        state: &StateHandle,
        label: String,
        mode_name: &str,
        work_mode: JsonValue,
        range: Option<Range<i64>>,
    ) -> Self {
        let command_topic = format!(
            "gv2mqtt/number/{id}/command/{mode}/{mode_num}",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name),
            mode_num = work_mode
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "work-mode-was-not-int".to_string()),
        );
        let state_topic = format!(
            "gv2mqtt/number/{id}/state/{mode}",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name)
        );

        let unique_id = format!(
            "gv2mqtt-{id}-{mode}-number",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name),
        );

        Self {
            number: NumberConfig {
                base: EntityConfig::for_device(device, Some(label), unique_id),
                command_topic,
                state_topic: Some(state_topic),
                min: range.as_ref().map(|r| r.start as f32).or(Some(0.)),
                max: range
                    .as_ref()
                    .map(|r| r.end.saturating_sub(1) as f32)
                    .or(Some(255.)),
                step: 1f32,
                unit_of_measurement: None,
                enabled_by_default: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            mode_name: mode_name.to_string(),
            work_mode,
        }
    }
}

#[async_trait]
impl EntityInstance for WorkModeNumber {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.number.publish(&state, &client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let state_topic = self
            .number
            .state_topic
            .as_ref()
            .ok_or_else(|| anyhow!("state_topic is None!?"))?;

        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "number entity").await
        else {
            return Ok(());
        };

        if let Some(cap) = device.get_state_capability_by_instance("workMode") {
            if let Some(work_mode) = cap.state.pointer("/value/workMode") {
                if *work_mode == self.work_mode {
                    // The current mode matches us, so it is valid to
                    // read the current parameter for that mode

                    if let Some(value) = cap.state.pointer("/value/modeValue") {
                        if let Some(n) = value.as_i64() {
                            client.publish(state_topic, n.to_string()).await?;
                            return Ok(());
                        }
                    }
                }
            }
        }

        if let Some(work_mode) = self.work_mode.as_i64() {
            // FIXME: assuming humidifier, rename that field?
            if let Some(n) = device.humidifier_param_by_mode.get(&(work_mode as u8)) {
                client.publish(state_topic, n.to_string()).await?;
                return Ok(());
            }
        }

        // We might get some data to report later, so this is just debug for now
        log::debug!(
            "Don't know how to report state for {} workMode {} value",
            self.device_id,
            self.mode_name
        );

        Ok(())
    }
}

pub struct MusicSensitivityNumber {
    number: NumberConfig,
    device_id: String,
    state: StateHandle,
}

impl MusicSensitivityNumber {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        let unique_id = format!("gv2mqtt-{id}-music-sensitivity", id = topic_safe_id(device));

        Self {
            number: NumberConfig {
                base: EntityConfig::for_device(
                    device,
                    Some("Music Sensitivity".to_string()),
                    unique_id,
                ),
                command_topic: format!(
                    "gv2mqtt/{id}/set-music-sensitivity",
                    id = topic_safe_id(device)
                ),
                state_topic: Some(format!(
                    "gv2mqtt/{id}/notify-music-sensitivity",
                    id = topic_safe_id(device)
                )),
                min: Some(0.0),
                max: Some(100.0),
                step: 1.0,
                unit_of_measurement: Some("%"),
                enabled_by_default: Some(false),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for MusicSensitivityNumber {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.number.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "music sensitivity number").await
        else {
            return Ok(());
        };

        let value = if let Some(cap) = device.get_state_capability_by_instance("musicMode") {
            cap.state
                .pointer("/value/sensitivity")
                .and_then(|value| value.as_u64())
                .map(|value| value.to_string())
        } else {
            device
                .active_music_mode()
                .map(|music| music.sensitivity.to_string())
        };

        if let Some(value) = value {
            return self.number.notify_state(client, &value).await;
        }

        Ok(())
    }
}

#[derive(Deserialize)]
pub struct IdAndModeName {
    id: String,
    mode_name: String,
    work_mode: String,
}

pub async fn mqtt_number_command(
    Payload(value): Payload<i64>,
    Params(IdAndModeName {
        id,
        mode_name,
        work_mode,
    }): Params<IdAndModeName>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("{mode_name} for {id}: {value}");
    let work_mode: i64 = work_mode.parse()?;
    let device = state.resolve_device_for_control(&id).await?;

    state
        .humidifier_set_parameter(&device, work_mode, value)
        .await?;

    Ok(())
}

pub async fn mqtt_set_music_sensitivity(
    Payload(value): Payload<u32>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;

    state.device_set_music_sensitivity(&device, value).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::MusicSensitivityNumber;
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::service::device::Device;
    use crate::service::hass::HassClient;
    use crate::service::state::State;
    use std::sync::Arc;

    #[test]
    fn music_sensitivity_number_has_expected_topics_and_registry_defaults() {
        let device = Device::new("H6000", "AA:BB");
        let state = Arc::new(State::new());
        let entity = MusicSensitivityNumber::new(&device, &state);

        assert_eq!(
            entity.number.base.name.as_deref(),
            Some("Music Sensitivity")
        );
        assert_eq!(
            entity.number.command_topic,
            "gv2mqtt/AABB/set-music-sensitivity"
        );
        assert_eq!(
            entity.number.state_topic.as_deref(),
            Some("gv2mqtt/AABB/notify-music-sensitivity")
        );
        assert_eq!(entity.number.min, Some(0.0));
        assert_eq!(entity.number.max, Some(100.0));
        assert_eq!(entity.number.unit_of_measurement, Some("%"));
        assert_eq!(entity.number.enabled_by_default, Some(false));

        let _entity_trait: &dyn EntityInstance = &entity;
    }

    #[tokio::test]
    async fn music_sensitivity_number_publishes_config_and_state_without_broker() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;

        {
            let mut device = state.device_mut("H6000", "AA:BB").await;
            device.set_active_music_mode("Spectrum", 42, true);
        }

        let device = state.device_by_id("AA:BB").await.unwrap();
        let entity = MusicSensitivityNumber::new(&device, &state);
        let client = HassClient::new_test();

        entity.publish_config(&state, &client).await.unwrap();
        entity.notify_state(&client).await.unwrap();

        let published = client.published_messages();
        assert_eq!(
            published[0].0,
            "homeassistant/number/gv2mqtt-AABB-music-sensitivity/config"
        );
        assert!(published[0].1.contains("\"enabled_by_default\":false"));
        assert_eq!(
            published[1],
            (
                "gv2mqtt/AABB/notify-music-sensitivity".to_string(),
                "42".to_string()
            )
        );
    }
}
