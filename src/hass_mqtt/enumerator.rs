use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::button::ButtonConfig;
use crate::hass_mqtt::climate::TargetTemperatureEntity;
use crate::hass_mqtt::humidifier::Humidifier;
use crate::hass_mqtt::instance::EntityList;
use crate::hass_mqtt::light::DeviceLight;
use crate::hass_mqtt::number::{MusicSensitivityNumber, WorkModeNumber};
use crate::hass_mqtt::scene::SceneConfig;
use crate::hass_mqtt::select::{
    EnumCapabilitySelect, MusicModeSelect, SceneModeSelect, WorkModeSelect,
};
use crate::hass_mqtt::sensor::{CapabilitySensor, DeviceStatusDiagnostic, GlobalFixedDiagnostic};
use crate::hass_mqtt::switch::{CapabilitySwitch, MusicAutoColorSwitch};
use crate::hass_mqtt::work_mode::ParsedWorkMode;
use crate::platform_api::{DeviceCapability, DeviceCapabilityKind, DeviceType};
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{availability_topic, oneclick_topic, purge_cache_topic};
use crate::service::state::StateHandle;
use crate::version_info::govee_version;
use anyhow::Context;

use uuid::Uuid;

pub async fn enumerate_all_entites(state: &StateHandle) -> anyhow::Result<EntityList> {
    let mut entities = EntityList::new();

    enumerate_global_entities(state, &mut entities).await?;
    enumerate_scenes(state, &mut entities).await?;

    let devices = state.devices().await;

    for d in &devices {
        enumerate_entities_for_device(d, state, &mut entities)
            .await
            .with_context(|| format!("Config::for_device({d})"))?;
    }

    // Enumerate device groups from config
    for (group_id, group) in crate::service::device_config::get_groups() {
        if group.members.is_empty() {
            log::warn!("Group '{group_id}' has no members, skipping");
            continue;
        }
        entities.add(crate::hass_mqtt::group_light::GroupLight::new(
            &group_id, &group, state,
        ));
    }

    Ok(entities)
}

async fn enumerate_global_entities(
    _state: &StateHandle,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    entities.add(GlobalFixedDiagnostic::new("Version", govee_version()));
    entities.add(ButtonConfig::new("Purge Caches", purge_cache_topic()));
    Ok(())
}

async fn enumerate_scenes(state: &StateHandle, entities: &mut EntityList) -> anyhow::Result<()> {
    if let Some(undoc) = state.get_undoc_client().await {
        match undoc.parse_one_clicks().await {
            Ok(items) => {
                for oc in items {
                    let unique_id = format!(
                        "gv2mqtt-one-click-{}",
                        Uuid::new_v5(&Uuid::NAMESPACE_DNS, oc.name.as_bytes()).simple()
                    );
                    entities.add(SceneConfig {
                        base: EntityConfig {
                            availability_topic: availability_topic(),
                            availability: vec![],
                            availability_mode: None,
                            name: Some(oc.name.to_string()),
                            entity_category: None,
                            origin: Origin::default(),
                            device: Device::this_service(),
                            unique_id: unique_id.clone(),
                            device_class: None,
                            icon: None,
                        },
                        command_topic: oneclick_topic(),
                        payload_on: oc.name,
                    });
                }
            }
            Err(err) => {
                log::warn!("Failed to parse one-clicks: {err:#}");
            }
        }
    }

    Ok(())
}

async fn entities_for_work_mode<'a>(
    d: &ServiceDevice,
    state: &StateHandle,
    cap: &DeviceCapability,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    let mut work_modes = ParsedWorkMode::with_capability(cap)?;
    work_modes.adjust_for_device(&d.sku);

    let quirk = d.resolve_quirk();

    for work_mode in work_modes.modes.values() {
        let Some(mode_num) = work_mode.value.as_i64() else {
            continue;
        };

        let range = work_mode.contiguous_value_range();

        let show_as_preset = work_mode.should_show_as_preset()
            || quirk
                .as_ref()
                .map(|q| q.should_show_mode_as_preset(&work_mode.name))
                .unwrap_or(false);

        if show_as_preset {
            if work_mode.values.is_empty() {
                entities.add(ButtonConfig::activate_work_mode_preset(
                    d,
                    &format!("Activate Mode: {}", work_mode.label()),
                    &work_mode.name,
                    mode_num,
                    work_mode.default_value(),
                ));
            } else {
                for value in &work_mode.values {
                    if let Some(mode_value) = value.value.as_i64() {
                        entities.add(ButtonConfig::activate_work_mode_preset(
                            d,
                            &value.computed_label,
                            &work_mode.name,
                            mode_num,
                            mode_value,
                        ));
                    }
                }
            }
        } else {
            let label = work_mode.label().to_string();

            entities.add(WorkModeNumber::new(
                d,
                state,
                label,
                &work_mode.name,
                work_mode.value.clone(),
                range,
            ));
        }
    }

    entities.add(WorkModeSelect::new(d, &work_modes, state));

    Ok(())
}

pub async fn enumerate_entities_for_device<'a>(
    d: &'a ServiceDevice,
    state: &StateHandle,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    if !d.is_controllable() {
        return Ok(());
    }

    entities.add(DeviceStatusDiagnostic::new(d, state));
    entities.add(ButtonConfig::request_platform_data_for_device(d));

    if d.supports_rgb() || d.get_color_temperature_range().is_some() || d.supports_brightness() {
        entities.add(DeviceLight::for_device(&d, state, None).await?);
    } else if let DeviceType::Other(ref other) = d.device_type() {
        log::info!(
            "Device {d} has unknown type '{other}'. \
             Exposing available capabilities (switches, sensors). \
             Use /api/device/{id}/inspect to see full device data.",
            id = crate::service::hass::topic_safe_id(d),
        );
    }

    if matches!(
        d.device_type(),
        DeviceType::Humidifier | DeviceType::Dehumidifier
    ) {
        entities.add(Humidifier::new(&d, state).await?);
    }

    let mut has_dedicated_scene_controls = false;

    if let Some(info) = &d.http_device_info {
        for (instance, label) in [
            ("lightScene", "Scene"),
            ("diyScene", "DIY Scene"),
            ("snapshot", "Snapshot"),
            ("nightlightScene", "Night Light Scene"),
        ] {
            if let Some(select) = EnumCapabilitySelect::new(d, state, instance, label).await? {
                has_dedicated_scene_controls = true;
                entities.add(select);
            }
        }

        if let Some(select) = MusicModeSelect::new(d, state).await? {
            has_dedicated_scene_controls = true;
            entities.add(select);
            entities.add(MusicSensitivityNumber::new(d, state));
            entities.add(MusicAutoColorSwitch::new(d, state));
        }

        for cap in &info.capabilities {
            match &cap.kind {
                DeviceCapabilityKind::Toggle | DeviceCapabilityKind::OnOff => {
                    entities.add(CapabilitySwitch::new(&d, state, cap).await?);
                }
                DeviceCapabilityKind::ColorSetting
                | DeviceCapabilityKind::SegmentColorSetting
                | DeviceCapabilityKind::MusicSetting
                | DeviceCapabilityKind::Event
                | DeviceCapabilityKind::Mode
                | DeviceCapabilityKind::DynamicScene => {}

                DeviceCapabilityKind::Range if cap.instance == "brightness" => {}
                DeviceCapabilityKind::Range if cap.instance == "humidity" => {}
                DeviceCapabilityKind::WorkMode => {
                    entities_for_work_mode(d, state, cap, entities).await?;
                }

                DeviceCapabilityKind::Property => {
                    entities.add(CapabilitySensor::new(&d, state, cap).await?);
                }

                DeviceCapabilityKind::TemperatureSetting => {
                    entities.add(TargetTemperatureEntity::new(&d, state, cap).await?);
                }

                kind => {
                    log::info!(
                        "Unhandled capability {kind:?} '{instance}' for {d}. \
                         If you need this capability, please open an issue with \
                         the output of /api/device/{id}/inspect",
                        instance = cap.instance,
                        id = crate::service::hass::topic_safe_id(d),
                    );
                }
            }
        }

        let segments = info.supports_segmented_rgb().or_else(|| {
            // Fall back to quirk-defined segment count when API doesn't report it
            d.resolve_quirk()
                .and_then(|q| q.segment_count)
                .map(|count| 0..count)
        });
        if let Some(segments) = segments {
            for n in segments {
                entities.add(DeviceLight::for_device(&d, state, Some(n)).await?);
            }
        }
    }

    if !matches!(d.device_type(), DeviceType::Light) || !has_dedicated_scene_controls {
        if let Some(scenes) = SceneModeSelect::new(d, state).await? {
            entities.add(scenes);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::enumerate_entities_for_device;
    use crate::cache::{cache_get, invalidate_key, CacheComputeResult, CacheGetOptions};
    use crate::hass_mqtt::instance::EntityList;
    use crate::platform_api::{
        DeviceCapability, DeviceCapabilityKind, DeviceParameters, DeviceType, EnumOption,
        GoveeApiClient, HttpDeviceInfo, IntegerRange, StructField,
    };
    use crate::service::device::Device;
    use crate::service::hass::HassClient;
    use crate::service::state::State;
    use crate::undoc_api::{LightEffectCategory, LightEffectEntry, LightEffectScene};
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn light_scene_select_is_published_when_catalog_has_scenes_even_without_capability() {
        let sku = "TEST_ENUMERATOR_LIGHT_SCENE";
        let cache_key = format!("scenes-{sku}");
        invalidate_key("undoc-api", &cache_key).ok();

        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: &cache_key,
                soft_ttl: Duration::from_secs(300),
                hard_ttl: Duration::from_secs(86400 * 7),
                negative_ttl: Duration::from_secs(1),
                allow_stale: true,
            },
            async {
                Ok(CacheComputeResult::Value(vec![LightEffectCategory {
                    category_id: 1,
                    category_name: "Life".to_string(),
                    scenes: vec![LightEffectScene {
                        scene_id: 7,
                        icon_urls: vec![],
                        scene_name: "Forest Glow".to_string(),
                        analytic_name: "forest_glow".to_string(),
                        scene_type: 0,
                        scene_code: 0,
                        scence_category_id: 1,
                        pop_up_prompt: 0,
                        scenes_hint: String::new(),
                        rule: json!({}),
                        light_effects: vec![LightEffectEntry {
                            scence_param_id: 42,
                            scence_name: "Forest Glow".to_string(),
                            scence_param: String::new(),
                            scene_code: 1,
                            special_effect: vec![],
                            cmd_version: None,
                            scene_type: 0,
                            diy_effect_code: vec![],
                            diy_effect_str: String::new(),
                            rules: vec![],
                            speed_info: json!({}),
                        }],
                        voice_url: String::new(),
                        create_time: 0,
                    }],
                }]))
            },
        )
        .await
        .unwrap();

        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;
        state
            .set_platform_client(GoveeApiClient::new("dummy").unwrap())
            .await;

        {
            let mut device = state.device_mut(sku, "AA:BB").await;
            *device = Device::new(sku, "AA:BB");
            device.set_http_device_info(HttpDeviceInfo {
                sku: sku.to_string(),
                device: "AA:BB".to_string(),
                device_name: "Catalog Lamp".to_string(),
                device_type: DeviceType::Light,
                capabilities: vec![],
            });
        }

        let device = state.device_by_id("AA:BB").await.unwrap();
        let mut entities = EntityList::new();
        enumerate_entities_for_device(&device, &state, &mut entities)
            .await
            .unwrap();

        let client = HassClient::new_test();
        entities.publish_config(&state, &client).await.unwrap();

        let published = client.published_messages();
        let scene_select = published
            .iter()
            .find(|(topic, payload)| {
                topic.ends_with("/gv2mqtt-AABB-lightscene-select/config")
                    && payload.contains("\"name\":\"Scene\"")
                    && payload.contains("Forest Glow")
            })
            .cloned();

        assert!(
            scene_select.is_some(),
            "expected dedicated Scene select config, got {published:#?}"
        );

        invalidate_key("undoc-api", &cache_key).ok();
    }

    #[tokio::test]
    async fn light_skips_mode_scene_when_dedicated_scene_controls_exist() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;

        {
            let mut device = state.device_mut("H6076", "AA:BB").await;
            *device = Device::new("H6076", "AA:BB");
            device.set_http_device_info(HttpDeviceInfo {
                sku: "H6076".to_string(),
                device: "AA:BB".to_string(),
                device_name: "Stick Lamp".to_string(),
                device_type: DeviceType::Light,
                capabilities: vec![
                    DeviceCapability {
                        kind: DeviceCapabilityKind::DynamicScene,
                        instance: "lightScene".to_string(),
                        parameters: Some(DeviceParameters::Enum {
                            options: vec![
                                EnumOption {
                                    name: "Aurora".to_string(),
                                    value: json!(1),
                                    extras: Default::default(),
                                },
                                EnumOption {
                                    name: "Sunrise".to_string(),
                                    value: json!(2),
                                    extras: Default::default(),
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
                            fields: vec![
                                StructField {
                                    field_name: "musicMode".to_string(),
                                    field_type: DeviceParameters::Enum {
                                        options: vec![EnumOption {
                                            name: "Dynamic".to_string(),
                                            value: json!(1),
                                            extras: Default::default(),
                                        }],
                                    },
                                    default_value: None,
                                    required: true,
                                },
                                StructField {
                                    field_name: "sensitivity".to_string(),
                                    field_type: DeviceParameters::Integer {
                                        unit: Some("unit.percent".to_string()),
                                        range: IntegerRange {
                                            min: 0,
                                            max: 100,
                                            precision: 1,
                                        },
                                    },
                                    default_value: Some(json!(100)),
                                    required: true,
                                },
                            ],
                        }),
                        alarm_type: None,
                        event_state: None,
                    },
                ],
            });
        }

        let device = state.device_by_id("AA:BB").await.unwrap();
        let mut entities = EntityList::new();
        enumerate_entities_for_device(&device, &state, &mut entities)
            .await
            .unwrap();

        let client = HassClient::new_test();
        entities.publish_config(&state, &client).await.unwrap();

        let published = client.published_messages();

        assert!(
            published.iter().any(|(topic, payload)| topic
                .ends_with("/gv2mqtt-AABB-lightscene-select/config")
                && payload.contains("\"name\":\"Scene\"")),
            "expected dedicated Scene select, got {published:#?}"
        );
        assert!(
            published.iter().any(|(topic, payload)| topic
                .ends_with("/gv2mqtt-AABB-music-mode-select/config")
                && payload.contains("\"name\":\"Music Mode\"")),
            "expected dedicated Music Mode select, got {published:#?}"
        );
        assert!(
            !published.iter().any(|(topic, payload)| topic
                .ends_with("/gv2mqtt-AABB-mode-scene/config")
                && payload.contains("\"name\":\"Mode/Scene\"")),
            "did not expect fallback Mode/Scene select when dedicated controls exist: {published:#?}"
        );
    }
}
