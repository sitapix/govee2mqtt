use crate::commands::serve::POLL_INTERVAL;
use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::humidifier::DEVICE_CLASS_HUMIDITY;
use crate::hass_mqtt::instance::{lookup_entity_device, publish_entity_config, EntityInstance};
use crate::platform_api::DeviceCapability;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    availability_topic, device_availability_entries, topic_safe_id, topic_safe_string, HassClient,
};
use crate::service::quirks::HumidityUnits;
use crate::service::state::StateHandle;
use crate::temperature::{TemperatureUnits, TemperatureValue, DEVICE_CLASS_TEMPERATURE};
use async_trait::async_trait;
use chrono::Utc;
use serde::Serialize;
use serde_json::json;

#[derive(Serialize, Clone, Debug)]
pub struct SensorConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub state_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_class: Option<StateClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_of_measurement: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_attributes_topic: Option<String>,
}

#[allow(unused)]
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateClass {
    #[serde(rename = "measurement")]
    Measurement,
    #[serde(rename = "total")]
    Total,
    #[serde(rename = "total_increasing")]
    TotalIncreasing,
}

impl SensorConfig {
    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("sensor", state, client, &self.base, self).await
    }

    pub async fn notify_state(&self, client: &HassClient, value: &str) -> anyhow::Result<()> {
        client.publish(&self.state_topic, value).await
    }
}

#[derive(Clone)]
pub struct GlobalFixedDiagnostic {
    sensor: SensorConfig,
    value: String,
}

#[async_trait]
impl EntityInstance for GlobalFixedDiagnostic {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.notify_state(client, &self.value).await
    }
}

impl GlobalFixedDiagnostic {
    pub fn new<NAME: Into<String>, VALUE: Into<String>>(name: NAME, value: VALUE) -> Self {
        let name = name.into();
        let unique_id = format!("global-{}", topic_safe_string(&name));

        Self {
            sensor: SensorConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    availability: vec![],
                    availability_mode: None,
                    name: Some(name),
                    entity_category: Some("diagnostic".to_string()),
                    origin: Origin::default(),
                    device: Device::this_service(),
                    unique_id: unique_id.clone(),
                    device_class: None,
                    icon: None,
                },
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class: None,
                unit_of_measurement: None,
                json_attributes_topic: None,
            },
            value: value.into(),
        }
    }
}

#[derive(Clone)]
pub struct CapabilitySensor {
    sensor: SensorConfig,
    device_id: String,
    state: StateHandle,
    instance_name: String,
}

impl CapabilitySensor {
    pub async fn new(
        device: &ServiceDevice,
        state: &StateHandle,
        instance: &DeviceCapability,
    ) -> anyhow::Result<Self> {
        let unique_id = format!(
            "sensor-{id}-{inst}",
            id = topic_safe_id(device),
            inst = topic_safe_string(&instance.instance)
        );

        let unit_of_measurement = match instance.instance.as_str() {
            "sensorTemperature" => Some(state.get_temperature_scale().await.unit_of_measurement()),
            "sensorHumidity" => Some("%"),
            "electricCurrent" => Some("A"),
            "electricPower" | "powerConsumption" => Some("W"),
            "voltage" => Some("V"),
            "energy" | "energyConsumption" => Some("kWh"),
            "carbonDioxideConcentration" | "carbonDioxide" | "co2" => Some("ppm"),
            "pm25Concentration" | "pm25" => Some("µg/m³"),
            "pm10Concentration" | "pm10" => Some("µg/m³"),
            _ => None,
        };

        let device_class = match instance.instance.as_str() {
            "sensorTemperature" => Some(DEVICE_CLASS_TEMPERATURE),
            "sensorHumidity" => Some(DEVICE_CLASS_HUMIDITY),
            "electricCurrent" => Some("current"),
            "electricPower" | "powerConsumption" => Some("power"),
            "voltage" => Some("voltage"),
            "energy" | "energyConsumption" => Some("energy"),
            "carbonDioxideConcentration" | "carbonDioxide" | "co2" => Some("carbon_dioxide"),
            "pm25Concentration" | "pm25" => Some("pm25"),
            "pm10Concentration" | "pm10" => Some("pm10"),
            _ => None,
        };

        let state_class = match instance.instance.as_str() {
            "sensorTemperature"
            | "sensorHumidity"
            | "carbonDioxideConcentration"
            | "carbonDioxide"
            | "co2"
            | "pm25Concentration"
            | "pm25"
            | "pm10Concentration"
            | "pm10" => Some(StateClass::Measurement),
            "electricCurrent" | "electricPower" | "powerConsumption" | "voltage" => {
                Some(StateClass::Measurement)
            }
            "energy" | "energyConsumption" => Some(StateClass::TotalIncreasing),
            _ => None,
        };

        let name = match instance.instance.as_str() {
            "sensorTemperature" => "Temperature".to_string(),
            "sensorHumidity" => "Humidity".to_string(),
            "online" => "Connected to Govee Cloud".to_string(),
            "electricCurrent" => "Current".to_string(),
            "electricPower" | "powerConsumption" => "Power".to_string(),
            "voltage" => "Voltage".to_string(),
            "energy" | "energyConsumption" => "Energy".to_string(),
            "carbonDioxideConcentration" | "carbonDioxide" | "co2" => "CO₂".to_string(),
            "pm25Concentration" | "pm25" => "PM2.5".to_string(),
            "pm10Concentration" | "pm10" => "PM10".to_string(),
            _ => crate::service::hass::camel_case_to_space_separated(&instance.instance),
        };

        // Primary entities (user-facing measurements) vs diagnostics.
        // Air-quality sensors are primary like power/energy.
        let entity_category = match instance.instance.as_str() {
            "electricCurrent"
            | "electricPower"
            | "powerConsumption"
            | "voltage"
            | "energy"
            | "energyConsumption"
            | "sensorTemperature"
            | "sensorHumidity"
            | "carbonDioxideConcentration"
            | "carbonDioxide"
            | "co2"
            | "pm25Concentration"
            | "pm25"
            | "pm10Concentration"
            | "pm10" => None,
            _ => Some("diagnostic".to_string()),
        };

        let (availability, availability_mode) = device_availability_entries(device);

        Ok(Self {
            sensor: SensorConfig {
                base: EntityConfig {
                    availability_topic: String::new(),
                    availability,
                    availability_mode,
                    name: Some(name),
                    entity_category,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: unique_id.clone(),
                    device_class,
                    icon: None,
                },
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class,
                unit_of_measurement,
                json_attributes_topic: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            instance_name: instance.instance.to_string(),
        })
    }
}

#[async_trait]
impl EntityInstance for CapabilitySensor {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "capability sensor").await
        else {
            return Ok(());
        };

        let quirk = device.resolve_quirk();

        if let Some(cap) = device.get_state_capability_by_instance(&self.instance_name) {
            let value = match self.instance_name.as_str() {
                "sensorTemperature" => {
                    let units = quirk
                        .and_then(|q| q.platform_temperature_sensor_units)
                        .unwrap_or(TemperatureUnits::Fahrenheit);

                    match cap
                        .state
                        .pointer("/value")
                        .and_then(|v| v.as_f64())
                        .map(|v| TemperatureValue::new(v, units))
                    {
                        Some(v) => {
                            let value = v
                                .as_unit(self.state.get_temperature_scale().await.into())
                                .value();
                            format!("{value:.2}")
                        }
                        None => "".to_string(),
                    }
                }
                "sensorHumidity" => {
                    let units = quirk
                        .and_then(|q| q.platform_humidity_sensor_units)
                        .unwrap_or(HumidityUnits::RelativePercent);
                    match cap
                        .state
                        .pointer("/value")
                        .and_then(|v| v.as_f64())
                        .map(|v| units.from_reading_to_relative_percent(v))
                    {
                        Some(v) => format!("{v:.2}"),
                        None => "".to_string(),
                    }
                }
                _ => cap.state.to_string(),
            };

            return self.sensor.notify_state(client, &value).await;
        }
        log::trace!(
            "CapabilitySensor::notify_state: didn't find state for {device} {instance}",
            instance = self.instance_name
        );
        Ok(())
    }
}

pub struct DeviceStatusDiagnostic {
    sensor: SensorConfig,
    device_id: String,
    state: StateHandle,
}

impl DeviceStatusDiagnostic {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        let unique_id = format!("sensor-{id}-gv2mqtt-status", id = topic_safe_id(device),);

        let (availability, availability_mode) = device_availability_entries(device);

        Self {
            sensor: SensorConfig {
                base: EntityConfig {
                    availability_topic: String::new(),
                    availability,
                    availability_mode,
                    name: Some("Status".to_string()),
                    entity_category: Some("diagnostic".to_string()),
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: unique_id.clone(),
                    device_class: None,
                    icon: None,
                },
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class: None,
                json_attributes_topic: Some(format!("gv2mqtt/sensor/{unique_id}/attributes")),
                unit_of_measurement: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for DeviceStatusDiagnostic {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "device status diagnostic").await
        else {
            return Ok(());
        };

        let iot_state = device.compute_iot_device_state();
        let lan_state = device.compute_lan_device_state();
        let http_state = device.compute_http_device_state();
        let platform_metadata = &device.http_device_info;
        let platform_state = &device.http_device_state;
        let device_state = device.device_state();

        let now = Utc::now();

        let threshold = *POLL_INTERVAL + chrono::Duration::seconds(30);

        let summary = match &device_state {
            Some(state) => {
                if now - state.updated > threshold {
                    "Missing".to_string()
                } else {
                    "Available".to_string()
                }
            }
            None => "Unknown".to_string(),
        };

        let attributes = json!({
            "iot": iot_state,
            "lan": lan_state,
            "http": http_state,
            "platform_metadata": platform_metadata,
            "platform_state": platform_state,
            "overall": device_state,
        });

        self.sensor.notify_state(client, &summary).await?;
        if let Some(topic) = &self.sensor.json_attributes_topic {
            client.publish_obj(topic, attributes).await?;
        }
        Ok(())
    }
}

/// Diagnostic sensor that exposes a device setting (battery %, wifi signal)
/// sourced from the undoc API's DeviceSettings struct. Not live — refreshed
/// only when the undoc device list is re-fetched — but better than nothing
/// for automations that check "is my H5179 still in range / battery full".
pub struct DeviceSettingDiagnostic {
    sensor: SensorConfig,
    device_id: String,
    state: StateHandle,
    field: DeviceSettingField,
    /// Suppress MQTT publishes when the value hasn't changed. Device
    /// settings update only on device-list re-fetch (~10 min), so the
    /// poll-cycle (~30 s) caller would otherwise republish ~20x uselessly.
    last_published: std::sync::Mutex<Option<i64>>,
}

#[derive(Clone, Copy, Debug)]
pub enum DeviceSettingField {
    Battery,
    WifiLevel,
}

impl DeviceSettingField {
    fn slug(self) -> &'static str {
        match self {
            Self::Battery => "battery",
            Self::WifiLevel => "wifi-signal",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Battery => "Battery",
            Self::WifiLevel => "Wi-Fi Signal",
        }
    }

    fn device_class(self) -> Option<&'static str> {
        match self {
            Self::Battery => Some("battery"),
            // HA's `signal_strength` device class expects dBm; Govee reports
            // Wi-Fi as a 0-100 percentage, so we expose it as a plain sensor
            // rather than trigger an HA validation warning.
            Self::WifiLevel => None,
        }
    }

    fn unit(self) -> Option<&'static str> {
        Some("%")
    }
}

impl DeviceSettingDiagnostic {
    /// Returns `None` when the device has no value for this field, so the
    /// enumerator can skip publishing an empty sensor without double-reading
    /// the undoc device info.
    pub fn for_device(
        device: &ServiceDevice,
        state: &StateHandle,
        field: DeviceSettingField,
    ) -> Option<Self> {
        let settings = &device
            .undoc_device_info
            .as_ref()?
            .entry
            .device_ext
            .device_settings;
        match field {
            DeviceSettingField::Battery => settings.battery?,
            DeviceSettingField::WifiLevel => settings.wifi_level?,
        };

        let unique_id = format!(
            "sensor-{id}-{slug}",
            id = topic_safe_id(device),
            slug = field.slug()
        );

        let mut base =
            EntityConfig::for_device(device, Some(field.name().to_string()), unique_id.clone());
        base.entity_category = Some("diagnostic".to_string());
        base.device_class = field.device_class();

        Some(Self {
            sensor: SensorConfig {
                base,
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class: Some(StateClass::Measurement),
                json_attributes_topic: None,
                unit_of_measurement: field.unit(),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            field,
            last_published: Default::default(),
        })
    }
}

#[async_trait]
impl EntityInstance for DeviceSettingDiagnostic {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) =
            lookup_entity_device(&self.state, &self.device_id, "device setting diagnostic").await
        else {
            return Ok(());
        };
        let Some(info) = device.undoc_device_info.as_ref() else {
            return Ok(());
        };
        let Some(value) = (match self.field {
            DeviceSettingField::Battery => info.entry.device_ext.device_settings.battery,
            DeviceSettingField::WifiLevel => info.entry.device_ext.device_settings.wifi_level,
        }) else {
            return Ok(());
        };

        {
            let mut last = self.last_published.lock().unwrap();
            if *last == Some(value) {
                return Ok(());
            }
            *last = Some(value);
        }

        self.sensor.notify_state(client, &value.to_string()).await
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceSettingField, DeviceStatusDiagnostic, GlobalFixedDiagnostic};
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::lan_api::{DeviceColor, DeviceStatus};
    use crate::service::hass::HassClient;
    use crate::service::state::State;
    use std::sync::Arc;

    #[tokio::test]
    async fn global_fixed_diagnostic_publishes_config_and_state_without_broker() {
        let state = Arc::new(State::new());
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;
        let sensor = GlobalFixedDiagnostic::new("Version", "1.2.3");
        let client = HassClient::new_test();

        sensor.publish_config(&state, &client).await.unwrap();
        sensor.notify_state(&client).await.unwrap();

        let published = client.published_messages();
        assert_eq!(published[0].0, "homeassistant/sensor/global-version/config");
        assert_eq!(
            published[1],
            (
                "gv2mqtt/sensor/global-version/state".to_string(),
                "1.2.3".to_string()
            )
        );
    }

    #[tokio::test]
    async fn device_status_diagnostic_publishes_summary_and_attributes_without_broker() {
        let state = Arc::new(State::new());
        {
            let mut device = state.device_mut("H6000", "AA:BB").await;
            device.set_lan_device_status(DeviceStatus {
                on: true,
                brightness: 100,
                color: DeviceColor { r: 1, g: 2, b: 3 },
                color_temperature_kelvin: 0,
            });
        }

        let device = state.device_by_id("AA:BB").await.unwrap();
        let sensor = DeviceStatusDiagnostic::new(&device, &state);
        let client = HassClient::new_test();

        sensor.notify_state(&client).await.unwrap();

        let published = client.published_messages();
        assert_eq!(
            published[0],
            (
                "gv2mqtt/sensor/sensor-AABB-gv2mqtt-status/state".to_string(),
                "Available".to_string()
            )
        );
        assert_eq!(
            published[1].0,
            "gv2mqtt/sensor/sensor-AABB-gv2mqtt-status/attributes".to_string()
        );
        assert!(published[1].1.contains("\"overall\""));
        assert!(published[1].1.contains("\"LAN API\""));
    }

    #[tokio::test]
    async fn capability_sensor_electric_power_has_correct_device_class_and_unit() {
        use super::{CapabilitySensor, StateClass};
        use crate::platform_api::{DeviceCapability, DeviceCapabilityKind};

        let state = Arc::new(State::new());
        {
            let mut device = state.device_mut("H7142", "PP:QQ").await;
            device.set_lan_device_status(DeviceStatus {
                on: true,
                brightness: 100,
                color: DeviceColor { r: 0, g: 0, b: 0 },
                color_temperature_kelvin: 0,
            });
        }

        let device = state.device_by_id("PP:QQ").await.unwrap();
        let cap = DeviceCapability {
            kind: DeviceCapabilityKind::Property,
            instance: "electricPower".into(),
            parameters: None,
            alarm_type: None,
            event_state: None,
        };

        let sensor = CapabilitySensor::new(&device, &state, &cap).await.unwrap();

        // Verify device_class is "power"
        assert_eq!(
            sensor.sensor.base.device_class,
            Some("power"),
            "electricPower should have device_class 'power'"
        );
        // Verify unit is "W"
        assert_eq!(
            sensor.sensor.unit_of_measurement,
            Some("W"),
            "electricPower should have unit 'W'"
        );
        // Verify state_class is Measurement
        assert_eq!(
            sensor.sensor.state_class,
            Some(StateClass::Measurement),
            "electricPower should have state_class Measurement"
        );
        // Energy sensors should NOT be diagnostic
        assert!(
            sensor.sensor.base.entity_category.is_none(),
            "electricPower should not be a diagnostic entity"
        );
    }

    /// HA's `signal_strength` device class expects dBm; Govee reports Wi-Fi
    /// as a 0-100 percentage. Exposing it with `signal_strength` and `%`
    /// triggers a validation warning, so the Wi-Fi field must have no
    /// device_class.
    #[test]
    fn device_setting_field_wifi_has_no_device_class() {
        assert_eq!(DeviceSettingField::WifiLevel.device_class(), None);
        assert_eq!(DeviceSettingField::WifiLevel.unit(), Some("%"));
    }

    #[test]
    fn device_setting_field_battery_uses_battery_device_class() {
        assert_eq!(DeviceSettingField::Battery.device_class(), Some("battery"));
        assert_eq!(DeviceSettingField::Battery.unit(), Some("%"));
    }
}
