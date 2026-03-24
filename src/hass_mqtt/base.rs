use crate::service::device::Device as ServiceDevice;
use crate::service::hass::topic_safe_id;
use crate::version_info::govee_version;
use serde::Serialize;

const MODEL: &str = "gv2mqtt";
const URL: &str = "https://github.com/sitapix/govee2mqtt";

#[derive(Serialize, Clone, Debug)]
pub struct AvailabilityEntry {
    pub topic: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct EntityConfig {
    /// Single availability topic. Used when `availability` list is empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub availability_topic: String,

    /// List of availability topics. When non-empty, `availability_topic` is skipped.
    /// HA checks all topics — entity is available only when ALL report "online".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub availability: Vec<AvailabilityEntry>,

    /// "all" = available when ALL topics say online. "any" = available when ANY does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability_mode: Option<String>,

    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_class: Option<&'static str>,
    pub origin: Origin,
    pub device: Device,
    pub unique_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl EntityConfig {
    /// Create an EntityConfig for a device with standard defaults.
    /// Sets availability from the device, origin, and device info.
    /// device_class, entity_category, and icon default to None.
    pub fn for_device(
        device: &ServiceDevice,
        name: impl Into<Option<String>>,
        unique_id: String,
    ) -> Self {
        let (availability, availability_mode) =
            crate::service::hass::device_availability_entries(device);
        Self {
            availability_topic: String::new(),
            availability,
            availability_mode,
            name: name.into(),
            device_class: None,
            origin: Origin::default(),
            device: Device::for_device(device),
            unique_id,
            entity_category: None,
            icon: None,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Origin {
    pub name: &'static str,
    pub sw_version: &'static str,
    pub url: &'static str,
}

impl Default for Origin {
    fn default() -> Self {
        Self {
            name: MODEL,
            sw_version: govee_version(),
            url: URL,
        }
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Device {
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sw_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_area: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_device: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<(String, String)>,
}

impl Device {
    pub fn for_device(device: &ServiceDevice) -> Self {
        Self {
            name: device.name(),
            manufacturer: "Govee".to_string(),
            model: device.sku.to_string(),
            sw_version: None,
            suggested_area: device.room_name().map(|s| s.to_string()),
            via_device: Some("gv2mqtt".to_string()),
            identifiers: vec![
                format!("gv2mqtt-{}", topic_safe_id(device)),
                /*
                device.computed_name(),
                device.id.to_string(),
                */
            ],
            connections: vec![],
        }
    }

    pub fn this_service() -> Self {
        Self {
            name: "Govee to MQTT".to_string(),
            manufacturer: "Wez Furlong".to_string(),
            model: "govee2mqtt".to_string(),
            sw_version: Some(govee_version().to_string()),
            suggested_area: None,
            via_device: None,
            identifiers: vec!["gv2mqtt".to_string()],
            connections: vec![],
        }
    }
}
