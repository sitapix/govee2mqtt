use anyhow::Context;
use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Per-device user overrides loaded from a JSON configuration file.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct DeviceOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_temp_range: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer_lan: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_effects: Option<bool>,
    /// Per-device allowlist of effect names to publish in HA MQTT discovery.
    /// When set (non-empty), only these effects will appear in `effect_list`.
    /// Overrides the global `GOVEE_ALLOWED_EFFECTS` env var for this device.
    /// Useful for keeping the Google Home SYNC payload under its size limit
    /// without losing scene control inside HA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_effects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct DeviceGroup {
    pub name: String,
    /// Device IDs that belong to this group
    pub members: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
pub struct DeviceConfigFile {
    #[serde(default)]
    pub devices: HashMap<String, DeviceOverride>,
    #[serde(default)]
    pub groups: HashMap<String, DeviceGroup>,
}

static CONFIG: once_cell::sync::Lazy<ArcSwap<DeviceConfigFile>> =
    once_cell::sync::Lazy::new(|| ArcSwap::new(Arc::new(DeviceConfigFile::default())));

/// Track file modification time for hot-reload.
static LAST_MODIFIED: std::sync::Mutex<Option<std::time::SystemTime>> = std::sync::Mutex::new(None);

fn config_path() -> PathBuf {
    let mut path = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    path.push("govee-device-config.json");
    path
}

fn read_config_from_disk() -> DeviceConfigFile {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<DeviceConfigFile>(&data) {
            Ok(config) => {
                log::info!(
                    "Loaded device config from {} ({} entries)",
                    path.display(),
                    config.devices.len()
                );
                // Track modification time
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(modified) = meta.modified() {
                        *LAST_MODIFIED.lock().unwrap() = Some(modified);
                    }
                }
                config
            }
            Err(err) => {
                log::error!("Failed to parse device config {}: {err:#}", path.display());
                DeviceConfigFile::default()
            }
        },
        Err(_) => {
            log::trace!("No device config file at {}", path.display());
            DeviceConfigFile::default()
        }
    }
}

/// Load the device config file. Called at startup.
pub fn load_device_config() {
    let config = read_config_from_disk();
    validate_config(&config);
    CONFIG.store(Arc::new(config));
}

/// Check if the config file has changed on disk and reload if so.
/// Called periodically (e.g., every poll cycle).
/// Returns true if the config was reloaded.
pub fn check_for_reload() -> bool {
    let path = config_path();
    let current_modified = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok());

    let last = *LAST_MODIFIED.lock().unwrap();

    match (current_modified, last) {
        (Some(current), Some(prev)) if current != prev => {
            log::info!("Device config file changed, reloading");
            let config = read_config_from_disk();
            validate_config(&config);
            CONFIG.store(Arc::new(config));
            true
        }
        (Some(_), None) => {
            // File appeared where there was none before
            log::info!("Device config file appeared, loading");
            let config = read_config_from_disk();
            validate_config(&config);
            CONFIG.store(Arc::new(config));
            true
        }
        _ => false,
    }
}

/// Get a snapshot of the current config.
pub fn current_config() -> Arc<DeviceConfigFile> {
    CONFIG.load_full()
}

/// Save a new config to disk and reload it into memory.
pub fn save_config(config: &DeviceConfigFile) -> anyhow::Result<()> {
    validate_config(config);

    let path = config_path();
    let json = serde_json::to_string_pretty(config)?;

    // Write atomically via temp file
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, json.as_bytes())
        .with_context(|| format!("writing temp config to {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("renaming temp config to {}", path.display()))?;

    // Update in-memory state
    CONFIG.store(Arc::new(config.clone()));

    // Track the new modification time so hot-reload doesn't re-read immediately
    if let Ok(meta) = std::fs::metadata(&path) {
        if let Ok(modified) = meta.modified() {
            *LAST_MODIFIED.lock().unwrap() = Some(modified);
        }
    }

    log::info!(
        "Device config saved to {} ({} devices, {} groups)",
        path.display(),
        config.devices.len(),
        config.groups.len()
    );

    Ok(())
}

fn validate_config(config: &DeviceConfigFile) {
    for (key, ovr) in &config.devices {
        if let Some((min, max)) = ovr.color_temp_range {
            if min >= max {
                log::warn!("Device config [{key}]: color_temp_range min ({min}) >= max ({max})");
            }
            if min < 1000 || max > 12000 {
                log::warn!(
                    "Device config [{key}]: color_temp_range ({min}, {max}) outside \
                     typical range 1000-12000K"
                );
            }
        }
        if let Some(ref icon) = ovr.icon {
            if !icon.starts_with("mdi:") {
                log::warn!("Device config [{key}]: icon \"{icon}\" doesn't start with \"mdi:\"");
            }
        }
    }
}

/// Get all configured device groups.
pub fn get_groups() -> Vec<(String, DeviceGroup)> {
    let config = CONFIG.load();
    config
        .groups
        .iter()
        .map(|(id, group)| (id.clone(), group.clone()))
        .collect()
}

/// Get the override for a device by its ID and SKU.
pub fn get_device_override(device_id: &str, sku: &str) -> Option<DeviceOverride> {
    let config = CONFIG.load();

    // Try exact device ID match first (case-insensitive)
    for (key, val) in &config.devices {
        if key.eq_ignore_ascii_case(device_id) {
            return Some(val.clone());
        }
    }

    // Fall back to SKU match (model-wide override)
    for (key, val) in &config.devices {
        if key.eq_ignore_ascii_case(sku) {
            return Some(val.clone());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_override_serialize_deserialize_round_trip() {
        let ovr = DeviceOverride {
            name: Some("My Light".into()),
            color_temp_range: Some((2000, 6500)),
            prefer_lan: Some(true),
            disable_effects: None,
            allowed_effects: Some(vec!["Sunrise".into(), "Sunset".into()]),
            room: Some("Living Room".into()),
            icon: Some("mdi:lightbulb".into()),
        };
        let json = serde_json::to_string(&ovr).unwrap();
        let parsed: DeviceOverride = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("My Light"));
        assert_eq!(parsed.color_temp_range, Some((2000, 6500)));
        assert_eq!(parsed.prefer_lan, Some(true));
        assert!(parsed.disable_effects.is_none());
        assert_eq!(
            parsed.allowed_effects.as_deref(),
            Some(&["Sunrise".to_string(), "Sunset".to_string()][..])
        );
        assert_eq!(parsed.room.as_deref(), Some("Living Room"));
        assert_eq!(parsed.icon.as_deref(), Some("mdi:lightbulb"));
    }

    #[test]
    fn device_override_skip_none_fields_in_serialization() {
        let ovr = DeviceOverride::default();
        let json = serde_json::to_string(&ovr).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn device_group_serialize_deserialize() {
        let group = DeviceGroup {
            name: "Kitchen Lights".into(),
            members: vec!["dev1".into(), "dev2".into()],
            room: Some("Kitchen".into()),
            icon: Some("mdi:ceiling-light".into()),
        };
        let json = serde_json::to_string(&group).unwrap();
        let parsed: DeviceGroup = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "Kitchen Lights");
        assert_eq!(parsed.members, vec!["dev1", "dev2"]);
        assert_eq!(parsed.room.as_deref(), Some("Kitchen"));
        assert_eq!(parsed.icon.as_deref(), Some("mdi:ceiling-light"));
    }

    #[test]
    fn device_config_file_serialize_deserialize() {
        let mut devices = HashMap::new();
        devices.insert(
            "AA:BB".into(),
            DeviceOverride {
                name: Some("Test".into()),
                ..Default::default()
            },
        );
        let mut groups = HashMap::new();
        groups.insert(
            "g1".into(),
            DeviceGroup {
                name: "Group 1".into(),
                members: vec!["AA:BB".into()],
                room: None,
                icon: None,
            },
        );
        let config = DeviceConfigFile { devices, groups };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: DeviceConfigFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.devices.len(), 1);
        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.devices["AA:BB"].name.as_deref(), Some("Test"));
    }

    #[test]
    fn config_file_missing_fields_uses_defaults() {
        // Empty object should parse with default empty maps
        let parsed: DeviceConfigFile = serde_json::from_str("{}").unwrap();
        assert!(parsed.devices.is_empty());
        assert!(parsed.groups.is_empty());

        // Only devices key
        let parsed: DeviceConfigFile = serde_json::from_str(r#"{"devices": {}}"#).unwrap();
        assert!(parsed.devices.is_empty());
        assert!(parsed.groups.is_empty());
    }

    #[test]
    fn validate_config_warns_on_invalid_color_temp_range() {
        // This test verifies that validate_config does not panic with
        // invalid color_temp_range values. The actual warnings go to the
        // log system; we just verify the function completes without error.
        let mut devices = HashMap::new();
        devices.insert(
            "dev1".into(),
            DeviceOverride {
                color_temp_range: Some((5000, 2000)), // min >= max
                ..Default::default()
            },
        );
        devices.insert(
            "dev2".into(),
            DeviceOverride {
                color_temp_range: Some((500, 15000)), // outside typical range
                ..Default::default()
            },
        );
        let config = DeviceConfigFile {
            devices,
            groups: HashMap::new(),
        };
        // Should not panic
        validate_config(&config);
    }

    #[test]
    fn validate_config_warns_on_non_mdi_icon() {
        let mut devices = HashMap::new();
        devices.insert(
            "dev1".into(),
            DeviceOverride {
                icon: Some("fa:lightbulb".into()), // not mdi:
                ..Default::default()
            },
        );
        devices.insert(
            "dev2".into(),
            DeviceOverride {
                icon: Some("mdi:lamp".into()), // valid
                ..Default::default()
            },
        );
        let config = DeviceConfigFile {
            devices,
            groups: HashMap::new(),
        };
        // Should not panic; the non-mdi icon triggers a log::warn
        validate_config(&config);
    }

    #[test]
    fn validate_config_accepts_valid_config() {
        let mut devices = HashMap::new();
        devices.insert(
            "dev1".into(),
            DeviceOverride {
                color_temp_range: Some((2000, 6500)),
                icon: Some("mdi:lightbulb".into()),
                ..Default::default()
            },
        );
        let config = DeviceConfigFile {
            devices,
            groups: HashMap::new(),
        };
        validate_config(&config);
    }
}
