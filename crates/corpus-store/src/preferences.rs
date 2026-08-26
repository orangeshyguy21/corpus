//! Durable operator application preferences.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::filesystem::atomic_write;
use crate::store::Store;
use crate::yaml;

/// Remembered UI choices (`store/app.yaml`), separate from corpus data.
///
/// Every field defaults independently so older, partial, or hand-edited files
/// cannot prevent the application from starting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPrefs {
    /// The model last selected in the management chat.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chat_model: String,
}

impl Store {
    fn prefs_path(&self) -> PathBuf {
        self.root().join("app.yaml")
    }

    /// Load preferences, falling back to defaults for missing, unreadable, or
    /// malformed data. Preferences must never keep the application closed.
    pub fn load_prefs(&self) -> AppPrefs {
        fs::read_to_string(self.prefs_path())
            .ok()
            .and_then(|raw| yaml::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Persist preferences with the store's atomic replacement primitive.
    pub fn save_prefs(&self, prefs: &AppPrefs) -> Result<()> {
        let path = self.prefs_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&path, yaml::to_string(prefs)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let world =
            std::env::temp_dir().join(format!("corpus-preferences-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    #[test]
    fn preferences_round_trip_in_the_selected_store() {
        let store = tmp_store("round-trip");
        let prefs = AppPrefs {
            chat_model: "mlx/qwen3.8".into(),
        };
        store.save_prefs(&prefs).unwrap();
        assert_eq!(store.load_prefs(), prefs);
        assert!(store.root().join("app.yaml").is_file());
    }

    #[test]
    fn malformed_preferences_fail_open_to_defaults() {
        let store = tmp_store("malformed");
        fs::create_dir_all(store.root()).unwrap();
        fs::write(store.root().join("app.yaml"), "chat_model: [").unwrap();
        assert_eq!(store.load_prefs(), AppPrefs::default());
    }
}
