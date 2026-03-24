use crate::hass_mqtt::base::EntityConfig;
use crate::hass_mqtt::instance::{lookup_entity_device, publish_entity_config, EntityInstance};
use crate::platform_api::DeviceType;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    kelvin_to_mired, light_segment_state_topic, light_state_topic,
    topic_safe_id, HassClient,
};
use crate::service::state::StateHandle;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// <https://www.home-assistant.io/integrations/light.mqtt/#json-schema>
#[derive(Serialize, Clone, Debug)]
pub struct LightConfig {
    #[serde(flatten)]
    pub base: EntityConfig,
    pub schema: String,

    pub command_topic: String,
    /// The docs say that this is optional, but hass errors out if
    /// it is not passed
    pub state_topic: String,
    pub optimistic: bool,
    pub supported_color_modes: Vec<String>,
    /// Flag that defines if the light supports brightness.
    #[serde(skip_serializing)]
    pub brightness: bool,
    /// Defines the maximum brightness value (i.e., 100%) of the MQTT device.
    pub brightness_scale: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Flag that defines if the light supports effects.
    pub effect: bool,
    /// The list of effects the light supports.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effect_list: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_mireds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_mireds: Option<u32>,

    pub payload_available: String,
}

impl LightConfig {
    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("light", state, client, &self.base, self).await
    }
}

#[derive(Clone)]
pub struct DeviceLight {
    light: LightConfig,
    device_id: String,
    state: StateHandle,
}

#[async_trait]
impl EntityInstance for DeviceLight {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.light.publish(&state, &client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        if self.light.optimistic {
            return Ok(());
        }

        let Some(device) = lookup_entity_device(&self.state, &self.device_id, "light entity").await
        else {
            return Ok(());
        };

        match device.device_state() {
            Some(device_state) => {
                log::trace!("LightConfig::notify_state: state is {device_state:?}");

                let is_on = device_state.light_on.unwrap_or(false);

                let light_state = if is_on {
                    let mut payload = Map::new();
                    payload.insert("state".to_string(), json!("ON"));

                    let color_mode = if self.supports_color_mode("rgb") && device_state.kelvin == 0
                    {
                        payload.insert(
                            "color".to_string(),
                            json!({
                                "r": device_state.color.r,
                                "g": device_state.color.g,
                                "b": device_state.color.b,
                            }),
                        );
                        Some("rgb")
                    } else if self.supports_color_mode("color_temp") && device_state.kelvin != 0 {
                        payload.insert(
                            "color_temp".to_string(),
                            json!(kelvin_to_mired(device_state.kelvin)),
                        );
                        Some("color_temp")
                    } else if self.supports_color_mode("brightness") {
                        Some("brightness")
                    } else if self.supports_color_mode("onoff") {
                        Some("onoff")
                    } else {
                        None
                    };

                    if let Some(color_mode) = color_mode {
                        payload.insert("color_mode".to_string(), json!(color_mode));
                    }

                    if self.light.brightness
                        && matches!(color_mode, Some("rgb" | "color_temp" | "brightness"))
                    {
                        payload.insert("brightness".to_string(), json!(device_state.brightness));
                    }

                    if let Some(scene) = &device_state.scene {
                        payload.insert("effect".to_string(), json!(scene));
                    }

                    Value::Object(payload)
                } else {
                    json!({"state":"OFF"})
                };

                client
                    .publish_obj(&self.light.state_topic, &light_state)
                    .await
            }
            None => {
                // TODO: mark as unavailable or something? Don't
                // want to prevent attempting to control it though,
                // as that could cause it to wake up.
                client
                    .publish_obj(&self.light.state_topic, &json!({"state":"OFF"}))
                    .await
            }
        }
    }
}

fn effects_disabled() -> bool {
    std::env::var("GOVEE_DISABLE_EFFECTS")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn filter_effects(scenes: Vec<String>) -> Vec<String> {
    let allowed = std::env::var("GOVEE_ALLOWED_EFFECTS").ok();
    match allowed {
        Some(allowed) if !allowed.trim().is_empty() => {
            let allowed: Vec<String> = allowed
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            scenes
                .into_iter()
                .filter(|s| {
                    s.is_empty() || allowed.iter().any(|a| a == &s.to_ascii_lowercase())
                })
                .collect()
        }
        _ => scenes,
    }
}

impl DeviceLight {
    fn supports_color_mode(&self, mode: &str) -> bool {
        self.light
            .supported_color_modes
            .iter()
            .any(|supported| supported == mode)
    }

    pub async fn for_device(
        device: &ServiceDevice,
        state: &StateHandle,
        segment: Option<u32>,
    ) -> anyhow::Result<Self> {
        let quirk = device.resolve_quirk();
        let device_type = device.device_type();

        let command_topic = match segment {
            None => format!("gv2mqtt/light/{id}/command", id = topic_safe_id(device)),
            Some(seg) => format!(
                "gv2mqtt/light/{id}/command/{seg}",
                id = topic_safe_id(device)
            ),
        };

        let icon = match segment {
            Some(_) => None,
            None => {
                // User config override > quirk > none
                let config_icon = crate::service::device_config::get_device_override(
                    &device.id,
                    &device.sku,
                )
                .and_then(|ovr| ovr.icon);

                config_icon.or_else(|| {
                    if device_type == DeviceType::Light {
                        quirk.as_ref().map(|q| q.icon.to_string())
                    } else {
                        None
                    }
                })
            }
        };

        let state_topic = match segment {
            Some(seg) => light_segment_state_topic(device, seg),
            None => light_state_topic(device),
        };
        let unique_id = format!(
            "gv2mqtt-{id}{seg}",
            id = topic_safe_id(device),
            seg = segment.map(|n| format!("-{n}")).unwrap_or(String::new())
        );

        let device_effects_disabled = crate::service::device_config::get_device_override(
            &device.id,
            &device.sku,
        )
        .and_then(|ovr| ovr.disable_effects)
        .unwrap_or(false);

        let effect_list = if segment.is_some() || effects_disabled() || device_effects_disabled {
            vec![]
        } else {
            match state.device_list_scenes(device).await {
                Ok(scenes) => filter_effects(scenes),
                Err(err) => {
                    log::error!("Unable to list scenes for {device}: {err:#}");
                    vec![]
                }
            }
        };

        let mut supported_color_modes = vec![];

        if segment.is_some() || device.supports_rgb() {
            supported_color_modes.push("rgb".to_string());
        }

        let (min_mireds, max_mireds) = if segment.is_some() {
            (None, None)
        } else if let Some((min, max)) = device.get_color_temperature_range() {
            supported_color_modes.push("color_temp".to_string());
            // Note that min and max are swapped by the translation
            // from kelvin to mired
            (Some(kelvin_to_mired(max)), Some(kelvin_to_mired(min)))
        } else {
            (None, None)
        };

        let brightness = segment.is_some() || device.supports_brightness();

        if brightness && supported_color_modes.is_empty() {
            supported_color_modes.push("brightness".to_string());
        }
        if supported_color_modes.is_empty() {
            supported_color_modes.push("onoff".to_string());
        }

        let name = match segment {
            Some(n) => Some(format!("Segment {:03}", n + 1)),
            None if device_type == DeviceType::Humidifier => Some("Night Light".to_string()),
            None => None,
        };

        Ok(Self {
            light: LightConfig {
                base: EntityConfig::for_device(device, name, unique_id),
                schema: "json".to_string(),
                command_topic,
                state_topic,
                supported_color_modes,
                brightness,
                brightness_scale: 100,
                effect: !effect_list.is_empty(),
                effect_list,
                payload_available: "online".to_string(),
                max_mireds,
                min_mireds,
                optimistic: segment.is_some(),
                icon,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceLight;
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::lan_api::{DeviceColor, DeviceStatus, LanDevice};
    use crate::platform_api::{
        DeviceCapability, DeviceCapabilityKind, DeviceParameters, DeviceType, HttpDeviceInfo,
        IntegerRange,
    };
    use crate::service::device::Device;
    use crate::service::hass::HassClient;
    use crate::service::state::State;
    use std::sync::Arc;

    fn http_device_info(capabilities: Vec<DeviceCapability>) -> HttpDeviceInfo {
        HttpDeviceInfo {
            sku: "H6000".to_string(),
            device: "AA:BB".to_string(),
            device_name: "Desk Lamp".to_string(),
            device_type: DeviceType::Light,
            capabilities,
        }
    }

    #[tokio::test]
    async fn device_light_publishes_config_and_state_without_broker() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;

        let device_id = "AA:BB";
        {
            let mut device = state.device_mut("H6000", device_id).await;
            device.set_lan_device(LanDevice {
                ip: "127.0.0.1".parse().unwrap(),
                device: device_id.to_string(),
                sku: "H6000".to_string(),
                ble_version_hard: "1.00.00".to_string(),
                ble_version_soft: "1.00.00".to_string(),
                wifi_version_hard: "1.00.00".to_string(),
                wifi_version_soft: "1.00.00".to_string(),
            });
            device.set_lan_device_status(DeviceStatus {
                on: true,
                brightness: 64,
                color: DeviceColor { r: 1, g: 2, b: 3 },
                color_temperature_kelvin: 0,
            });
            device.set_active_scene(Some("Sunrise"));
        }

        let device = state.device_by_id(device_id).await.unwrap();
        let light = DeviceLight::for_device(&device, &state, None)
            .await
            .unwrap();
        let client = HassClient::new_test();

        light.publish_config(&state, &client).await.unwrap();
        light.notify_state(&client).await.unwrap();

        let published = client.published_messages();
        assert_eq!(published.len(), 2);
        assert_eq!(
            published[0].0,
            "homeassistant/light/gv2mqtt-AABB/config".to_string()
        );
        assert!(published[0]
            .1
            .contains("\"command_topic\":\"gv2mqtt/light/AABB/command\""));
        assert!(published[0]
            .1
            .contains("\"state_topic\":\"gv2mqtt/light/AABB/state\""));
        assert_eq!(published[1].0, "gv2mqtt/light/AABB/state".to_string());
        assert!(published[1].1.contains("\"state\":\"ON\""));
        assert!(published[1].1.contains("\"brightness\":64"));
        assert!(published[1].1.contains("\"effect\":\"Sunrise\""));
    }

    #[tokio::test]
    async fn device_light_falls_back_to_brightness_mode_without_deprecated_flag() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;

        {
            let mut device = state.device_mut("H6000", "AA:BB").await;
            device.set_http_device_info(http_device_info(vec![DeviceCapability {
                kind: DeviceCapabilityKind::Range,
                instance: "brightness".to_string(),
                parameters: Some(DeviceParameters::Integer {
                    unit: Some("unit.percent".to_string()),
                    range: IntegerRange {
                        min: 0,
                        max: 100,
                        precision: 1,
                    },
                }),
                alarm_type: None,
                event_state: None,
            }]));
            device.set_lan_device_status(DeviceStatus {
                on: true,
                brightness: 42,
                color: DeviceColor { r: 4, g: 5, b: 6 },
                color_temperature_kelvin: 0,
            });
        }

        let device = state.device_by_id("AA:BB").await.unwrap();
        let light = DeviceLight::for_device(&device, &state, None)
            .await
            .unwrap();
        let client = HassClient::new_test();

        light.publish_config(&state, &client).await.unwrap();
        light.notify_state(&client).await.unwrap();

        let published = client.published_messages();
        assert!(published[0]
            .1
            .contains("\"supported_color_modes\":[\"brightness\"]"));
        assert!(!published[0].1.contains("\"brightness\":"));
        assert!(published[1].1.contains("\"color_mode\":\"brightness\""));
        assert!(published[1].1.contains("\"brightness\":42"));
    }

    #[tokio::test]
    async fn device_light_falls_back_to_onoff_mode() {
        let state = Arc::new(State::new());
        {
            let mut device = state.device_mut("H6000", "AA:CC").await;
            device.set_lan_device_status(DeviceStatus {
                on: true,
                brightness: 12,
                color: DeviceColor { r: 7, g: 8, b: 9 },
                color_temperature_kelvin: 0,
            });
        }

        let device = state.device_by_id("AA:CC").await.unwrap();
        let light = DeviceLight::for_device(&device, &state, None)
            .await
            .unwrap();
        let client = HassClient::new_test();

        light.notify_state(&client).await.unwrap();

        let published = client.published_messages();
        assert_eq!(light.light.supported_color_modes, vec!["onoff".to_string()]);
        assert!(published[0].1.contains("\"color_mode\":\"onoff\""));
        assert!(!published[0].1.contains("\"brightness\":"));
    }

    #[tokio::test]
    async fn device_light_notify_state_is_noop_when_device_is_missing() {
        let state = Arc::new(State::new());
        let light = DeviceLight::for_device(&Device::new("H6000", "AA:DD"), &state, None)
            .await
            .unwrap();
        let client = HassClient::new_test();

        light.notify_state(&client).await.unwrap();

        assert!(client.published_messages().is_empty());
    }
}
