//! Decoded scene database — pre-captured ptReal command payloads per SKU.
//!
//! Scene data sourced from AlgoClaw/Govee decoded scene database.
//! These provide full scene parity for supported SKUs by sending
//! the exact same binary commands the Govee app sends, over LAN.
//!
//! Users can also add their own scene databases by placing JSON files
//! in the `scene-data/` directory (relative to XDG_CACHE_HOME).

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Deserialize, Debug, Clone)]
pub struct DecodedScene {
    pub name: String,
    pub cmd_b64: Vec<String>,
}

type SceneDb = HashMap<String, Vec<DecodedScene>>;

static DB: OnceLock<SceneDb> = OnceLock::new();

fn bundled_scene_dir() -> PathBuf {
    // Look next to the executable first, then in the working directory
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("scene-data")));

    if let Some(dir) = &exe_dir {
        if dir.exists() {
            return dir.clone();
        }
    }

    PathBuf::from("scene-data")
}

fn user_scene_dir() -> PathBuf {
    let mut path = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    path.push("scene-data");
    path
}

/// Load all scene databases from bundled and user directories.
pub fn load_scene_databases() {
    let mut db = SceneDb::new();

    for dir in [bundled_scene_dir(), user_scene_dir()] {
        if !dir.exists() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            // Extract SKU from filename (e.g., "H6076.json" or "H6076_final.json")
            let sku = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.split('_').next().unwrap_or(s))
                .unwrap_or("")
                .to_uppercase();

            if sku.is_empty() {
                continue;
            }

            match std::fs::read_to_string(&path) {
                Ok(data) => match serde_json::from_str::<Vec<DecodedScene>>(&data) {
                    Ok(scenes) => {
                        log::info!(
                            "Loaded {} decoded scenes for {} from {}",
                            scenes.len(),
                            sku,
                            path.display()
                        );
                        db.insert(sku, scenes);
                    }
                    Err(err) => {
                        log::warn!("Failed to parse scene database {}: {err:#}", path.display());
                    }
                },
                Err(err) => {
                    log::warn!("Failed to read {}: {err:#}", path.display());
                }
            }
        }
    }

    if !db.is_empty() {
        log::info!(
            "Scene database: {} SKUs, {} total scenes",
            db.len(),
            db.values().map(|v| v.len()).sum::<usize>()
        );
    }

    DB.set(db).ok();
}

/// Get decoded scene names for a given SKU.
pub fn scene_names_for_sku(sku: &str) -> Vec<String> {
    DB.get()
        .and_then(|db| db.get(&sku.to_uppercase()))
        .map(|scenes| scenes.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default()
}

/// Get the ptReal commands for a scene by SKU and name.
pub fn scene_commands(sku: &str, scene_name: &str) -> Option<Vec<String>> {
    DB.get()
        .and_then(|db| db.get(&sku.to_uppercase()))
        .and_then(|scenes| {
            scenes
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(scene_name))
                .map(|s| s.cmd_b64.clone())
        })
}

/// Check if a SKU has decoded scenes available.
pub fn has_scenes_for_sku(sku: &str) -> bool {
    DB.get()
        .and_then(|db| db.get(&sku.to_uppercase()))
        .map(|scenes| !scenes.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_scene_deserializes() {
        let json = r#"[
            {"name": "Movie", "cmd_b64": ["owABBQI=", "MwUEXwg="]},
            {"name": "Forest", "cmd_b64": ["owABBwI="]}
        ]"#;
        let scenes: Vec<DecodedScene> = serde_json::from_str(json).unwrap();
        assert_eq!(scenes.len(), 2);
        assert_eq!(scenes[0].name, "Movie");
        assert_eq!(scenes[0].cmd_b64.len(), 2);
        assert_eq!(scenes[1].name, "Forest");
    }

    #[test]
    fn scene_names_returns_empty_for_unknown_sku() {
        // DB may or may not be initialized depending on test order
        let names = scene_names_for_sku("NONEXISTENT_SKU_999");
        assert!(names.is_empty());
    }

    #[test]
    fn scene_commands_returns_none_for_unknown() {
        let cmds = scene_commands("NONEXISTENT_SKU_999", "Movie");
        assert!(cmds.is_none());
    }
}
