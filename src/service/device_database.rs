use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Persisted device metadata learned from Govee APIs.
/// Survives cache clears, container restarts, and API outages.
/// Enables the service to start in degraded LAN-only mode when APIs are down.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PersistedDevice {
    pub sku: String,
    pub device_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    /// When this entry was last updated from a live API response
    pub last_updated: String,
}

#[derive(Deserialize, Serialize, Debug, Default)]
pub struct DeviceDatabase {
    pub version: u32,
    /// Keyed by device ID
    pub devices: BTreeMap<String, PersistedDevice>,
}

fn database_path() -> PathBuf {
    let mut path = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    path.push("govee-devices.json");
    path
}

/// Load the persisted device database.
pub fn load_device_database() -> DeviceDatabase {
    let path = database_path();
    match std::fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str(&data) {
            Ok(db) => {
                let db: DeviceDatabase = db;
                log::info!(
                    "Loaded device database from {} ({} devices)",
                    path.display(),
                    db.devices.len()
                );
                db
            }
            Err(err) => {
                log::warn!(
                    "Failed to parse device database {}: {err:#}",
                    path.display()
                );
                DeviceDatabase::default()
            }
        },
        Err(_) => {
            log::trace!("No device database at {}", path.display());
            DeviceDatabase::default()
        }
    }
}

/// Save the device database atomically (write to temp, then rename).
pub fn save_device_database(db: &DeviceDatabase) -> anyhow::Result<()> {
    let path = database_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(db)?;
    std::fs::write(&tmp_path, data)?;
    std::fs::rename(&tmp_path, &path)?;

    log::trace!(
        "Saved device database to {} ({} devices)",
        path.display(),
        db.devices.len()
    );
    Ok(())
}

/// Update the database with current in-memory device state.
pub fn update_database_from_devices(
    db: &mut DeviceDatabase,
    devices: &[crate::service::device::Device],
) {
    let now = chrono::Utc::now().to_rfc3339();

    for device in devices {
        // Only persist devices we have real metadata for
        if device.http_device_info.is_none() && device.undoc_device_info.is_none() {
            continue;
        }

        let device_type = format!("{:?}", device.device_type());

        db.devices.insert(
            device.id.clone(),
            PersistedDevice {
                sku: device.sku.clone(),
                device_id: device.id.clone(),
                name: device.name(),
                room: device.room_name(),
                device_type: Some(device_type),
                last_updated: now.clone(),
            },
        );
    }

    db.version = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_device_database() -> DeviceDatabase {
        let mut db = DeviceDatabase {
            version: 1,
            ..Default::default()
        };
        db.devices.insert(
            "AA:BB:CC".into(),
            PersistedDevice {
                sku: "H6001".into(),
                device_id: "AA:BB:CC".into(),
                name: "Test Light".into(),
                room: Some("Bedroom".into()),
                device_type: Some("Light".into()),
                last_updated: "2024-01-01T00:00:00Z".into(),
            },
        );
        db
    }

    #[test]
    fn serialize_deserialize_round_trip() {
        let db = sample_device_database();
        let json = serde_json::to_string_pretty(&db).unwrap();
        let parsed: DeviceDatabase = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.devices.len(), 1);
        let dev = &parsed.devices["AA:BB:CC"];
        assert_eq!(dev.sku, "H6001");
        assert_eq!(dev.name, "Test Light");
        assert_eq!(dev.room.as_deref(), Some("Bedroom"));
        assert_eq!(dev.device_type.as_deref(), Some("Light"));
    }

    #[test]
    fn default_database_is_empty() {
        let db = DeviceDatabase::default();
        assert_eq!(db.version, 0);
        assert!(db.devices.is_empty());
    }

    #[test]
    fn update_database_from_devices_populates_entries() {
        let mut db = DeviceDatabase::default();
        let mut device = crate::service::device::Device::new("H7012", "DD:EE:FF");
        // Give it http_device_info so update_database_from_devices doesn't skip it
        device.http_device_info = Some(crate::platform_api::HttpDeviceInfo {
            sku: "H7012".into(),
            device: "DD:EE:FF".into(),
            device_name: "My Humidifier".into(),
            device_type: crate::platform_api::DeviceType::Other("humidifier".into()),
            capabilities: vec![],
        });
        update_database_from_devices(&mut db, &[device]);

        assert_eq!(db.version, 1);
        assert_eq!(db.devices.len(), 1);
        let entry = &db.devices["DD:EE:FF"];
        assert_eq!(entry.sku, "H7012");
        assert_eq!(entry.device_id, "DD:EE:FF");
    }

    #[test]
    fn update_database_skips_devices_without_metadata() {
        let mut db = DeviceDatabase::default();
        // Device with no http_device_info and no undoc_device_info
        let device = crate::service::device::Device::new("H6001", "XX:YY");
        update_database_from_devices(&mut db, &[device]);
        assert!(db.devices.is_empty());
    }

    #[test]
    fn save_and_load_round_trip_via_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        // Point database_path() at our temp dir by setting XDG_CACHE_HOME
        // We use a scoped approach: set it, run, then restore.
        let original = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", &dir_path);

        let db = sample_device_database();
        save_device_database(&db).unwrap();

        let loaded = load_device_database();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices["AA:BB:CC"].name, "Test Light");

        // Restore
        match original {
            Some(val) => std::env::set_var("XDG_CACHE_HOME", val),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
    }
}
