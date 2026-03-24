use crate::hass_mqtt::base::EntityConfig;
use crate::hass_mqtt::instance::{lookup_entity_device, publish_entity_config, EntityInstance};
use crate::hass_mqtt::work_mode::ParsedWorkMode;
use crate::platform_api::DeviceParameters;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    topic_safe_id, topic_safe_string, HassClient, IdParameter,
};
use crate::service::state::StateHandle;
use anyhow::Context;
use mosquitto_rs::router::{Params, Payload, State};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize, Clone, Debug)]
pub struct SelectConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub command_topic: String,
    pub options: Vec<String>,
    pub state_topic: String,
}

impl SelectConfig {
    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("select", state, client, &self.base, self).await
    }
}

pub struct WorkModeSelect {
    select: SelectConfig,
    device_id: String,
    state: StateHandle,
}

impl WorkModeSelect {
    pub fn new(device: &ServiceDevice, work_modes: &ParsedWorkMode, state: &StateHandle) -> Self {
        let command_topic = format!("gv2mqtt/{id}/set-work-mode", id = topic_safe_id(device),);
        let state_topic = format!("gv2mqtt/{id}/notify-work-mode", id = topic_safe_id(device));
        let unique_id = format!("gv2mqtt-{id}-workMode", id = topic_safe_id(device),);

        Self {
            select: SelectConfig {
                base: EntityConfig::for_device(device, Some("Mode".to_string()), unique_id),
                command_topic,
                state_topic,
                options: work_modes.get_mode_names(),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait::async_trait]
impl EntityInstance for WorkModeSelect {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.select.publish(&state, &client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "work mode select").await
        else {
            return Ok(());
        };

        if let Some(mode_value) = device.humidifier_work_mode {
            if let Ok(work_mode) = ParsedWorkMode::with_device(&device) {
                let mode_value_json = json!(mode_value);
                if let Some(mode) = work_mode.mode_for_value(&mode_value_json) {
                    client
                        .publish(&self.select.state_topic, mode.name.to_string())
                        .await?;
                }
            }
        } else {
            let work_modes = ParsedWorkMode::with_device(&device)?;

            if let Some(cap) = device.get_state_capability_by_instance("workMode") {
                if let Some(mode_num) = cap.state.pointer("/value/workMode") {
                    if let Some(mode) = work_modes.mode_for_value(mode_num) {
                        return client
                            .publish(&self.select.state_topic, mode.name.to_string())
                            .await;
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct SceneModeSelect {
    select: SelectConfig,
    device_id: String,
    state: StateHandle,
}

impl SceneModeSelect {
    pub async fn new(device: &ServiceDevice, state: &StateHandle) -> anyhow::Result<Option<Self>> {
        let scenes = state.device_list_scenes(device).await?;
        if scenes.is_empty() {
            return Ok(None);
        }

        let command_topic = format!("gv2mqtt/{id}/set-mode-scene", id = topic_safe_id(device));
        let state_topic = format!("gv2mqtt/{id}/notify-mode-scene", id = topic_safe_id(device));
        let unique_id = format!("gv2mqtt-{id}-mode-scene", id = topic_safe_id(device));

        Ok(Some(Self {
            select: SelectConfig {
                base: EntityConfig::for_device(device, Some("Mode/Scene".to_string()), unique_id),
                command_topic,
                state_topic,
                options: scenes,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl EntityInstance for SceneModeSelect {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.select.publish(&state, &client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "scene mode select").await
        else {
            return Ok(());
        };

        if let Some(device_state) = device.device_state() {
            client
                .publish(
                    &self.select.state_topic,
                    device_state.scene.as_deref().unwrap_or(""),
                )
                .await?;
        }

        Ok(())
    }
}

fn select_state_name_for_enum_value(
    device: &ServiceDevice,
    instance: &str,
    value: &serde_json::Value,
) -> Option<String> {
    let cap = device.get_capability_by_instance(instance)?;
    let DeviceParameters::Enum { options } = cap.parameters.as_ref()? else {
        return None;
    };

    options
        .iter()
        .find(|opt| opt.value == *value)
        .map(|opt| opt.name.to_string())
}

fn select_state_name_for_struct_enum_field(
    device: &ServiceDevice,
    instance: &str,
    field_name: &str,
    value: &serde_json::Value,
) -> Option<String> {
    let cap = device.get_capability_by_instance(instance)?;
    let field = cap.struct_field_by_name(field_name)?;
    let DeviceParameters::Enum { options } = &field.field_type else {
        return None;
    };

    options
        .iter()
        .find(|opt| opt.value == *value)
        .map(|opt| opt.name.to_string())
}

pub struct EnumCapabilitySelect {
    select: SelectConfig,
    device_id: String,
    state: StateHandle,
    instance_name: String,
}

impl EnumCapabilitySelect {
    pub async fn new(
        device: &ServiceDevice,
        state: &StateHandle,
        instance_name: &str,
        label: &str,
    ) -> anyhow::Result<Option<Self>> {
        let options = state
            .device_list_capability_options(device, instance_name)
            .await?;
        if options.is_empty() {
            return Ok(None);
        }

        let command_topic = format!(
            "gv2mqtt/{id}/set-capability-option/{instance}",
            id = topic_safe_id(device),
            instance = topic_safe_string(instance_name)
        );
        let state_topic = format!(
            "gv2mqtt/{id}/notify-capability-option/{instance}",
            id = topic_safe_id(device),
            instance = topic_safe_string(instance_name)
        );
        let unique_id = format!(
            "gv2mqtt-{id}-{instance}-select",
            id = topic_safe_id(device),
            instance = topic_safe_string(instance_name)
        );

        Ok(Some(Self {
            select: SelectConfig {
                base: EntityConfig::for_device(device, Some(label.to_string()), unique_id),
                command_topic,
                options,
                state_topic,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            instance_name: instance_name.to_string(),
        }))
    }
}

#[async_trait::async_trait]
impl EntityInstance for EnumCapabilitySelect {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.select.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "enum capability select").await
        else {
            return Ok(());
        };

        let selected =
            if let Some(cap) = device.get_state_capability_by_instance(&self.instance_name) {
                cap.state.pointer("/value").and_then(|value| {
                    select_state_name_for_enum_value(&device, &self.instance_name, value)
                })
            } else {
                match device.active_scene_instance() {
                    Some(instance) if instance.eq_ignore_ascii_case(&self.instance_name) => {
                        device.active_scene_name().map(str::to_string)
                    }
                    None => device.active_scene_name().and_then(|name| {
                        self.select
                            .options
                            .iter()
                            .find(|option| option.eq_ignore_ascii_case(name))
                            .cloned()
                    }),
                    _ => None,
                }
            };

        client
            .publish(&self.select.state_topic, selected.unwrap_or_default())
            .await
    }
}

pub struct MusicModeSelect {
    select: SelectConfig,
    device_id: String,
    state: StateHandle,
}

impl MusicModeSelect {
    pub async fn new(device: &ServiceDevice, state: &StateHandle) -> anyhow::Result<Option<Self>> {
        let options = state.device_list_music_modes(device).await?;
        if options.is_empty() {
            return Ok(None);
        }

        let command_topic = format!("gv2mqtt/{id}/set-music-mode", id = topic_safe_id(device));
        let state_topic = format!("gv2mqtt/{id}/notify-music-mode", id = topic_safe_id(device));
        let unique_id = format!("gv2mqtt-{id}-music-mode-select", id = topic_safe_id(device));

        Ok(Some(Self {
            select: SelectConfig {
                base: EntityConfig::for_device(device, Some("Music Mode".to_string()), unique_id),
                command_topic,
                options,
                state_topic,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl EntityInstance for MusicModeSelect {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.select.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "music mode select").await
        else {
            return Ok(());
        };

        let selected = if let Some(cap) = device.get_state_capability_by_instance("musicMode") {
            cap.state.pointer("/value/musicMode").and_then(|value| {
                select_state_name_for_struct_enum_field(&device, "musicMode", "musicMode", value)
            })
        } else {
            device
                .active_music_mode()
                .map(|music| music.mode.to_string())
                .or_else(|| {
                    device
                        .active_scene_name()
                        .and_then(|scene| scene.strip_prefix("Music: ").map(str::to_string))
                })
        };

        client
            .publish(&self.select.state_topic, selected.unwrap_or_default())
            .await
    }
}

pub async fn mqtt_set_mode_scene(
    Payload(scene): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;

    if let Err(err) = state.device_set_scene(&device, &scene).await {
        let msg = format!("Scene '{scene}' failed for {device}: {err:#}");
        log::error!("{msg}");
        if let Some(hass) = state.get_hass_client().await {
            let _ = hass
                .publish("gv2mqtt/bridge/error", &msg)
                .await;
        }
        return Err(err).context("mqtt_set_mode_scene");
    }

    Ok(())
}

#[derive(serde::Deserialize)]
pub struct IdAndInstance {
    id: String,
    instance: String,
}

pub async fn mqtt_set_capability_option(
    Payload(option): Payload<String>,
    Params(IdAndInstance { id, instance }): Params<IdAndInstance>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;

    state
        .device_set_capability_option(&device, &instance, &option)
        .await
        .with_context(|| format!("mqtt_set_capability_option: {instance} -> {option}"))?;

    Ok(())
}

pub async fn mqtt_set_music_mode(
    Payload(mode): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;

    state
        .device_set_music_mode(&device, &mode)
        .await
        .context("mqtt_set_music_mode: state.device_set_music_mode")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{select_state_name_for_enum_value, select_state_name_for_struct_enum_field};
    use crate::hass_mqtt::base::{Device as HassDevice, EntityConfig, Origin};
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::platform_api::{
        DeviceCapability, DeviceCapabilityKind, DeviceParameters, DeviceType, EnumOption,
        HttpDeviceInfo, HttpDeviceState, StructField,
    };
    use crate::service::device::Device;
    use crate::service::hass::HassClient;
    use crate::service::state::State;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_device() -> Device {
        let mut device = Device::new("H6000", "aa:bb");
        device.set_http_device_info(HttpDeviceInfo {
            sku: "H6000".to_string(),
            device: "aa:bb".to_string(),
            device_name: "Desk Lamp".to_string(),
            device_type: DeviceType::Light,
            capabilities: vec![
                DeviceCapability {
                    kind: DeviceCapabilityKind::Mode,
                    instance: "nightlightScene".to_string(),
                    parameters: Some(DeviceParameters::Enum {
                        options: vec![
                            EnumOption {
                                name: "Forest".to_string(),
                                value: json!(1),
                                extras: HashMap::new(),
                            },
                            EnumOption {
                                name: "Aurora".to_string(),
                                value: json!(2),
                                extras: HashMap::new(),
                            },
                        ],
                    }),
                    alarm_type: None,
                    event_state: None,
                },
                DeviceCapability {
                    kind: DeviceCapabilityKind::MusicSetting,
                    instance: "musicMode".to_string(),
                    parameters: Some(DeviceParameters::Struct {
                        fields: vec![StructField {
                            field_name: "musicMode".to_string(),
                            field_type: DeviceParameters::Enum {
                                options: vec![
                                    EnumOption {
                                        name: "Rhythm".to_string(),
                                        value: json!(1),
                                        extras: HashMap::new(),
                                    },
                                    EnumOption {
                                        name: "Spectrum".to_string(),
                                        value: json!(2),
                                        extras: HashMap::new(),
                                    },
                                ],
                            },
                            default_value: None,
                            required: true,
                        }],
                    }),
                    alarm_type: None,
                    event_state: None,
                },
            ],
        });
        device.set_http_device_state(HttpDeviceState {
            sku: "H6000".to_string(),
            device: "aa:bb".to_string(),
            capabilities: vec![],
        });
        device
    }

    #[test]
    fn enum_select_maps_state_value_to_option_name() {
        let device = test_device();
        let selected =
            select_state_name_for_enum_value(&device, "nightlightScene", &json!(2)).unwrap();

        assert_eq!(selected, "Aurora");
    }

    #[test]
    fn music_mode_select_maps_struct_field_value_to_option_name() {
        let device = test_device();
        let selected =
            select_state_name_for_struct_enum_field(&device, "musicMode", "musicMode", &json!(1))
                .unwrap();

        assert_eq!(selected, "Rhythm");
    }

    #[tokio::test]
    async fn music_mode_select_publishes_config_and_state_without_broker() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;

        {
            let mut device = state.device_mut("H6000", "AA:BB").await;
            *device = test_device();
            device.id = "AA:BB".to_string();
            device.set_active_music_mode("Spectrum", 55, false);
        }

        let device = state.device_by_id("AA:BB").await.unwrap();
        let select = super::MusicModeSelect {
            select: super::SelectConfig {
                base: EntityConfig {
                    availability_topic: crate::service::hass::availability_topic(),
                    availability: vec![],
                    availability_mode: None,
                    name: Some("Music Mode".to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: HassDevice::for_device(&device),
                    unique_id: "gv2mqtt-AABB-music-mode-select".to_string(),
                    entity_category: None,
                    icon: None,
                },
                command_topic: "gv2mqtt/AABB/set-music-mode".to_string(),
                options: vec!["Rhythm".to_string(), "Spectrum".to_string()],
                state_topic: "gv2mqtt/AABB/notify-music-mode".to_string(),
            },
            device_id: "AA:BB".to_string(),
            state: state.clone(),
        };
        let client = HassClient::new_test();

        select.publish_config(&state, &client).await.unwrap();
        select.notify_state(&client).await.unwrap();

        let published = client.published_messages();
        assert_eq!(
            published[0].0,
            "homeassistant/select/gv2mqtt-AABB-music-mode-select/config"
        );
        assert!(published[0]
            .1
            .contains("\"options\":[\"Rhythm\",\"Spectrum\"]"));
        assert_eq!(
            published[1],
            (
                "gv2mqtt/AABB/notify-music-mode".to_string(),
                "Spectrum".to_string()
            )
        );
    }
}
