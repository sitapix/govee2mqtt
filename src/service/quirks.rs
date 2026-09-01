use crate::platform_api::DeviceType;
use crate::temperature::TemperatureUnits;
use once_cell::sync::Lazy;
use std::borrow::Cow;
use std::collections::HashMap;

#[allow(unused)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HumidityUnits {
    RelativePercent,
    RelativePercentTimes100,
}

impl HumidityUnits {
    #[allow(clippy::wrong_self_convention)]
    pub fn from_reading_to_relative_percent(&self, value: f64) -> f64 {
        match self {
            Self::RelativePercent => value,
            Self::RelativePercentTimes100 => value / 100.,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Quirk {
    pub sku: Cow<'static, str>,
    pub icon: Cow<'static, str>,
    pub supports_rgb: bool,
    pub supports_brightness: bool,
    pub color_temp_range: Option<(u32, u32)>,
    pub avoid_platform_api: bool,
    pub ble_only: bool,
    pub lan_api_capable: bool,
    pub device_type: DeviceType,
    pub platform_temperature_sensor_units: Option<TemperatureUnits>,
    pub platform_humidity_sensor_units: Option<HumidityUnits>,
    /// If true, we can correctly parse all appropriate
    /// packets from the MQTT subscription and apply
    /// their state.
    pub iot_api_supported: bool,
    pub show_as_preset_buttons: Option<&'static [&'static str]>,
    /// Number of controllable segments, if the Platform API doesn't report them.
    pub segment_count: Option<u32>,
}

impl Quirk {
    pub fn device<SKU: Into<Cow<'static, str>>>(
        sku: SKU,
        device_type: DeviceType,
        icon: &'static str,
    ) -> Self {
        Self {
            sku: sku.into(),
            supports_rgb: false,
            supports_brightness: false,
            color_temp_range: None,
            avoid_platform_api: false,
            ble_only: false,
            icon: icon.into(),
            lan_api_capable: false,
            device_type,
            platform_temperature_sensor_units: None,
            platform_humidity_sensor_units: None,
            iot_api_supported: false,
            show_as_preset_buttons: None,
            segment_count: None,
        }
    }

    pub fn light<SKU: Into<Cow<'static, str>>>(sku: SKU, icon: &'static str) -> Self {
        Self::device(sku, DeviceType::Light, icon)
            .with_rgb()
            .with_brightness()
            .with_color_temp()
            .with_iot_api_support(true)
    }

    pub fn ice_maker<SKU: Into<Cow<'static, str>>>(sku: SKU) -> Self {
        Self::device(sku, DeviceType::IceMaker, "mdi:snowflake")
    }

    pub fn space_heater<SKU: Into<Cow<'static, str>>>(sku: SKU) -> Self {
        Self::device(sku, DeviceType::Heater, "mdi:heat-wave")
    }

    pub fn fan<SKU: Into<Cow<'static, str>>>(sku: SKU) -> Self {
        Self::device(sku, DeviceType::Fan, "mdi:fan")
    }

    pub fn humidifier<SKU: Into<Cow<'static, str>>>(sku: SKU) -> Self {
        Self::device(sku, DeviceType::Humidifier, "mdi:air-humidifier")
    }

    pub fn thermometer<SKU: Into<Cow<'static, str>>>(sku: SKU) -> Self {
        Self::device(sku, DeviceType::Thermometer, "mdi:thermometer")
    }

    pub fn air_quality_monitor<SKU: Into<Cow<'static, str>>>(sku: SKU) -> Self {
        Self::device(sku, DeviceType::AirQualityMonitor, "mdi:molecule-co2")
    }

    pub fn with_rgb(mut self) -> Self {
        self.supports_rgb = true;
        self
    }

    pub fn with_brightness(mut self) -> Self {
        self.supports_brightness = true;
        self
    }

    pub fn with_platform_temperature_sensor_units(mut self, units: TemperatureUnits) -> Self {
        self.platform_temperature_sensor_units = Some(units);
        self
    }

    pub fn with_platform_humidity_sensor_units(mut self, units: HumidityUnits) -> Self {
        self.platform_humidity_sensor_units = Some(units);
        self
    }

    pub fn with_iot_api_support(mut self, supported: bool) -> Self {
        self.iot_api_supported = supported;
        self
    }

    pub fn with_color_temp(mut self) -> Self {
        self.color_temp_range = Some((2000, 9000));
        self
    }

    pub fn with_color_temp_range(mut self, min: u32, max: u32) -> Self {
        self.color_temp_range = Some((min, max));
        self
    }

    pub fn with_lan_api(mut self) -> Self {
        self.lan_api_capable = true;
        self
    }

    pub fn with_show_as_preset_modes(mut self, modes: &'static [&'static str]) -> Self {
        self.show_as_preset_buttons.replace(modes);
        self
    }

    pub fn with_broken_platform(mut self) -> Self {
        self.avoid_platform_api = true;
        self
    }

    pub fn with_segment_count(mut self, count: u32) -> Self {
        self.segment_count = Some(count);
        self
    }

    pub fn with_ble_only(mut self, ble_only: bool) -> Self {
        self.ble_only = ble_only;
        self
    }

    pub fn lan_api_capable_light(sku: &'static str, icon: &'static str) -> Self {
        Self::light(sku, icon).with_lan_api()
    }

    pub fn should_show_mode_as_preset(&self, mode: &str) -> bool {
        self.show_as_preset_buttons
            .as_ref()
            .map(|modes| modes.contains(&mode))
            .unwrap_or(false)
    }
}

static QUIRKS: Lazy<HashMap<String, Quirk>> = Lazy::new(|| {
    let mut map = load_quirks();
    load_external_quirks(&mut map);
    map
});

/// A user-defined quirk loaded from JSON.
#[derive(serde::Deserialize, Debug)]
struct ExternalQuirk {
    pub sku: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default)]
    pub supports_rgb: bool,
    #[serde(default)]
    pub supports_brightness: bool,
    #[serde(default)]
    pub color_temp_range: Option<(u32, u32)>,
    #[serde(default)]
    pub avoid_platform_api: bool,
    #[serde(default)]
    pub ble_only: bool,
    #[serde(default)]
    pub lan_api_capable: bool,
    #[serde(default = "default_device_type")]
    pub device_type: String,
    #[serde(default)]
    pub iot_api_supported: bool,
}

fn default_icon() -> String {
    BULB.to_string()
}
fn default_device_type() -> String {
    "light".to_string()
}

fn parse_device_type(s: &str) -> DeviceType {
    match s.to_ascii_lowercase().as_str() {
        "light" => DeviceType::Light,
        "humidifier" => DeviceType::Humidifier,
        "dehumidifier" => DeviceType::Dehumidifier,
        "heater" => DeviceType::Heater,
        "thermometer" => DeviceType::Thermometer,
        "kettle" => DeviceType::Kettle,
        "ice_maker" | "icemaker" => DeviceType::IceMaker,
        other => DeviceType::Other(other.to_string()),
    }
}

fn external_quirks_path() -> std::path::PathBuf {
    let mut path = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push("govee-quirks.json");
    path
}

fn load_external_quirks(map: &mut HashMap<String, Quirk>) {
    let path = external_quirks_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let externals: Vec<ExternalQuirk> = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(err) => {
            log::error!(
                "Failed to parse external quirks {}: {err:#}",
                path.display()
            );
            return;
        }
    };

    log::info!(
        "Loaded {} external quirk(s) from {}",
        externals.len(),
        path.display()
    );

    for ext in externals {
        let quirk = Quirk {
            sku: Cow::Owned(ext.sku.clone()),
            icon: Cow::Owned(ext.icon),
            supports_rgb: ext.supports_rgb,
            supports_brightness: ext.supports_brightness,
            color_temp_range: ext.color_temp_range,
            avoid_platform_api: ext.avoid_platform_api,
            ble_only: ext.ble_only,
            lan_api_capable: ext.lan_api_capable,
            device_type: parse_device_type(&ext.device_type),
            platform_temperature_sensor_units: None,
            platform_humidity_sensor_units: None,
            iot_api_supported: ext.iot_api_supported,
            show_as_preset_buttons: None,
            segment_count: None,
        };
        if map.contains_key(&ext.sku) {
            log::info!("External quirk overrides built-in for SKU {}", ext.sku);
        }
        map.insert(ext.sku, quirk);
    }
}

const STRIP: &str = "mdi:led-strip-variant";
const STRIP_ALT: &str = "mdi:led-strip";
const FLOOD: &str = "mdi:light-flood-down";
const STRING: &str = "mdi:string-lights";
pub const BULB: &str = "mdi:lightbulb";
const FLOOR_LAMP: &str = "mdi:floor-lamp";
const TV_BACK: &str = "mdi:television-ambient-light";
const DESK: &str = "mdi:desk-lamp";
const HEX: &str = "mdi:hexagon-multiple";
const TRIANGLE: &str = "mdi:triangle";
const CEILING: &str = "mdi:ceiling-light";
const NIGHTLIGHT: &str = "mdi:lightbulb-night";
const WALL_SCONCE: &str = "mdi:wall-sconce";
const OUTDOOR_LAMP: &str = "mdi:outdoor-lamp";
const SPOTLIGHT: &str = "mdi:lightbulb-spot";

fn load_quirks() -> HashMap<String, Quirk> {
    let mut map = HashMap::new();
    for quirk in [
        // H60A1 Govee Ceiling Light has a color temperature range of 2200K - 6500K
        // Without this quirk, the LAN API fallback reports (2000, 9000) which causes issues
        // <https://github.com/wez/govee2mqtt/pull/502>
        Quirk::lan_api_capable_light("H60A1", CEILING).with_color_temp_range(2200, 6500),
        // Color temperature is more restrictive than the fallback range
        // <https://github.com/wez/govee2mqtt/issues/511>
        Quirk::lan_api_capable_light("H6022", BULB).with_color_temp_range(2700, 6500),
        Quirk::lan_api_capable_light("H610A", STRIP),
        // At the time of writing, the metadata
        // returned by Govee is completely bogus for this
        // device
        // <https://github.com/wez/govee2mqtt/issues/15>
        Quirk::light("H6141", STRIP).with_broken_platform(),
        // At the time of writing, the metadata
        // returned by Govee is completely bogus for this
        // device
        // <https://github.com/wez/govee2mqtt/issues/14#issuecomment-1880050091>
        Quirk::light("H6159", STRIP).with_broken_platform(),
        // <https://github.com/wez/govee2mqtt/issues/152>
        Quirk::light("H6003", BULB).with_broken_platform(),
        // H6006 WiFi bulbs supports IoT but lacks a quirk; without it, commands
        // route through the rate-limited Platform API (10-15s delays).
        // <https://github.com/wez/govee2mqtt/issues/621>
        Quirk::light("H6006", BULB).with_iot_api_support(true),
        // <https://github.com/wez/govee2mqtt/issues/40#issuecomment-1889726710>
        // indicates that this one doesn't work like the others with IoT
        Quirk::light("H6121", STRIP).with_iot_api_support(false),
        // <https://github.com/wez/govee2mqtt/issues/40>
        Quirk::light("H6154", STRIP).with_iot_api_support(false),
        // <https://github.com/wez/govee2mqtt/issues/49>
        Quirk::light("H6176", STRIP).with_iot_api_support(false),
        // Platform API probably shouldn't return this device (I suppose,
        // aside from letting us find out its name), and we need to know
        // that it is definitely BLE-only
        // <https://github.com/wez/govee2mqtt/issues/92>
        Quirk::light("H6102", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        // Another BLE-only device <https://github.com/wez/govee2mqtt/issues/77>
        Quirk::light("H6053", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        Quirk::light("H617C", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        Quirk::light("H617E", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        Quirk::light("H617F", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        Quirk::light("H6119", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        // Humidifer with mangled platform API data
        Quirk::humidifier("H7160")
            .with_broken_platform()
            .with_iot_api_support(true)
            .with_rgb()
            .with_brightness(),
        Quirk::space_heater("H7130")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::space_heater("H7131")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_show_as_preset_modes(&["gearMode"])
            .with_rgb()
            .with_brightness(),
        Quirk::space_heater("H713A")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::space_heater("H713B")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::space_heater("H7132")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::space_heater("H7133")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_show_as_preset_modes(&["gearMode"])
            .with_rgb()
            .with_brightness(),
        Quirk::space_heater("H7134")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_show_as_preset_modes(&["gearMode"])
            .with_color_temp()
            .with_brightness(),
        Quirk::space_heater("H7135")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        // <https://github.com/wez/govee2mqtt/issues/343>
        Quirk::ice_maker("H7172").with_iot_api_support(false),
        // Tower fan, IoT only (from 64bitjoe fork)
        Quirk::fan("H7105").with_iot_api_support(true),
        Quirk::thermometer("H5051")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_platform_humidity_sensor_units(HumidityUnits::RelativePercent),
        Quirk::thermometer("H5100")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_platform_humidity_sensor_units(HumidityUnits::RelativePercent),
        Quirk::thermometer("H5103")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_platform_humidity_sensor_units(HumidityUnits::RelativePercent),
        Quirk::thermometer("H5179")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_platform_humidity_sensor_units(HumidityUnits::RelativePercent),
        Quirk::device("H7170", DeviceType::Kettle, "mdi:kettle")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::device("H7171", DeviceType::Kettle, "mdi:kettle")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_show_as_preset_modes(&["M1", "M2", "M3", "M4"]),
        Quirk::device("H7173", DeviceType::Kettle, "mdi:kettle")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_show_as_preset_modes(&["Tea", "Coffee", "DIY"]),
        // Lights from the list of LAN API enabled devices
        // at <https://app-h5.govee.com/user-manual/wlan-guide>
        Quirk::lan_api_capable_light("H6072", FLOOR_LAMP),
        Quirk::lan_api_capable_light("H619B", STRIP),
        Quirk::lan_api_capable_light("H619C", STRIP),
        Quirk::lan_api_capable_light("H619Z", STRIP),
        Quirk::lan_api_capable_light("H7060", FLOOD),
        Quirk::lan_api_capable_light("H6046", TV_BACK),
        Quirk::lan_api_capable_light("H6047", TV_BACK),
        Quirk::lan_api_capable_light("H6051", DESK),
        Quirk::lan_api_capable_light("H6056", STRIP_ALT),
        Quirk::lan_api_capable_light("H6059", NIGHTLIGHT),
        Quirk::lan_api_capable_light("H6061", HEX),
        Quirk::lan_api_capable_light("H6062", STRIP),
        Quirk::lan_api_capable_light("H6065", STRIP),
        Quirk::lan_api_capable_light("H6066", HEX),
        Quirk::lan_api_capable_light("H6067", TRIANGLE),
        Quirk::lan_api_capable_light("H6073", FLOOR_LAMP),
        // Color temp clamp: API reports 2000-9009K but real range is 2700-6500K
        // <https://github.com/wez/govee2mqtt/issues/591>
        Quirk::lan_api_capable_light("H6076", FLOOR_LAMP).with_color_temp_range(2700, 6500),
        Quirk::lan_api_capable_light("H6078", FLOOR_LAMP),
        Quirk::lan_api_capable_light("H6087", WALL_SCONCE),
        // Neon Rope Light 2, LAN-capable (from homeassilol fork)
        Quirk::lan_api_capable_light("H60B0", STRIP),
        Quirk::lan_api_capable_light("H610A", STRIP),
        Quirk::lan_api_capable_light("H610B", STRIP),
        Quirk::lan_api_capable_light("H6117", STRIP),
        Quirk::lan_api_capable_light("H6159", STRIP),
        Quirk::lan_api_capable_light("H615E", STRIP),
        Quirk::lan_api_capable_light("H6163", STRIP),
        Quirk::lan_api_capable_light("H6168", TV_BACK),
        Quirk::lan_api_capable_light("H6172", STRIP),
        Quirk::lan_api_capable_light("H6173", STRIP),
        Quirk::lan_api_capable_light("H618A", STRIP),
        Quirk::lan_api_capable_light("H618C", STRIP),
        Quirk::lan_api_capable_light("H618E", STRIP),
        Quirk::lan_api_capable_light("H618F", STRIP),
        Quirk::lan_api_capable_light("H619A", STRIP),
        Quirk::lan_api_capable_light("H619D", STRIP),
        Quirk::lan_api_capable_light("H619E", STRIP),
        Quirk::lan_api_capable_light("H61A0", STRIP),
        Quirk::lan_api_capable_light("H61A1", STRIP),
        Quirk::lan_api_capable_light("H61A2", STRIP),
        Quirk::lan_api_capable_light("H61A3", STRIP),
        Quirk::lan_api_capable_light("H61A5", STRIP),
        Quirk::lan_api_capable_light("H61A8", STRIP),
        Quirk::lan_api_capable_light("H61A9", STRIP),
        Quirk::lan_api_capable_light("H61B2", TV_BACK),
        Quirk::lan_api_capable_light("H61E1", STRIP),
        // COB Strip Light Pro 9.8ft — color temp 2700-6500K (API reports 2000-9000K).
        // Segment count over-reported (16) vs actual (12); leaving segments alone since
        // the user notes extras can be ignored. <https://github.com/wez/govee2mqtt/issues/567>
        Quirk::lan_api_capable_light("H61E5", STRIP).with_color_temp_range(2700, 6500),
        Quirk::lan_api_capable_light("H66A1", TV_BACK),
        Quirk::lan_api_capable_light("H7012", STRING),
        Quirk::lan_api_capable_light("H7013", STRING),
        Quirk::lan_api_capable_light("H7021", STRING),
        Quirk::lan_api_capable_light("H7028", STRING),
        Quirk::lan_api_capable_light("H7041", STRING),
        Quirk::lan_api_capable_light("H7042", STRING),
        // Desk light, LAN-capable (from faxd fork)
        Quirk::lan_api_capable_light("H8022", DESK),
        // H7050/H7051 support segments in the Govee app but the Platform API
        // doesn't report segmentedColorRgb. Segment count from user reports.
        // <https://github.com/wez/govee2mqtt/issues/559>
        Quirk::lan_api_capable_light("H7050", BULB).with_segment_count(15),
        Quirk::lan_api_capable_light("H7051", BULB).with_segment_count(15),
        Quirk::lan_api_capable_light("H7052", STRING),
        Quirk::lan_api_capable_light("H7055", BULB),
        Quirk::lan_api_capable_light("H705A", OUTDOOR_LAMP),
        Quirk::lan_api_capable_light("H705B", OUTDOOR_LAMP),
        Quirk::lan_api_capable_light("H7061", FLOOD),
        Quirk::lan_api_capable_light("H7062", FLOOD),
        Quirk::lan_api_capable_light("H7065", SPOTLIGHT),
        // Outdoor strip 2 x 7.5m RGBIC — <https://github.com/wez/govee2mqtt/issues/577>
        Quirk::lan_api_capable_light("H616D", STRIP),
        // Wall sconce, LAN-capable — <https://github.com/wez/govee2mqtt/issues/605>
        Quirk::lan_api_capable_light("H6039", WALL_SCONCE),
        // H5140 Smart CO₂ Monitor — reports as devices.types.air_quality_monitor
        // with carbonDioxideConcentration / sensorTemperature / sensorHumidity.
        // <https://github.com/wez/govee2mqtt/issues/634>
        Quirk::air_quality_monitor("H5140")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_platform_humidity_sensor_units(HumidityUnits::RelativePercent)
            .with_iot_api_support(true),
        // H5106 BLE-only air quality monitor. <https://github.com/wez/govee2mqtt/issues/561>
        Quirk::air_quality_monitor("H5106")
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit)
            .with_platform_humidity_sensor_units(HumidityUnits::RelativePercent)
            .with_ble_only(true)
            .with_iot_api_support(true),
        // BLE-only LED strips — without a BLE ingest path we can't drive
        // them, but classifying them as lights silences the "unknown device
        // type" warnings users see in logs. If/when BLE control lands,
        // these already report the right capabilities.
        // <https://github.com/wez/govee2mqtt/issues/569> H6125_5321
        // <https://github.com/wez/govee2mqtt/issues/630> H6125_321A
        Quirk::light("H6125_321A", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        Quirk::light("H6125_5321", STRIP)
            .with_broken_platform()
            .with_ble_only(true),
        // H5129 outdoor motion sensor — BLE only. Same caveat as the
        // strips above: classified correctly but not controllable until
        // a BLE path exists. <https://github.com/wez/govee2mqtt/issues/580>
        Quirk::device("H5129", DeviceType::Sensor, "mdi:motion-sensor").with_ble_only(true),
        // Meat thermometers (BLE-only). Mapping them keeps the log clean
        // and establishes the right icon; real values require BLE ingest.
        Quirk::thermometer("H5181")
            .with_ble_only(true)
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::thermometer("H5182")
            .with_ble_only(true)
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::thermometer("H5183")
            .with_ble_only(true)
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::thermometer("H5184")
            .with_ble_only(true)
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::thermometer("H5185")
            .with_ble_only(true)
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
        Quirk::thermometer("H5198")
            .with_ble_only(true)
            .with_platform_temperature_sensor_units(TemperatureUnits::Fahrenheit),
    ] {
        map.insert(quirk.sku.to_string(), quirk);
    }

    map
}

pub fn resolve_quirk(sku: &str) -> Option<&'static Quirk> {
    QUIRKS.get(sku)
}
