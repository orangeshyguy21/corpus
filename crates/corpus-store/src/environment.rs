//! Durable environment-session identity and lease/provenance record.
//!
//! These are store data only. Process execution and plugin protocol calls stay
//! in corpus-core, while every frontend shares the same crash-recoverable
//! identity and record shape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Error, Result, Store};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EnvironmentSessionId {
    pub project: String,
    pub mission: String,
    pub generation: u64,
}

impl EnvironmentSessionId {
    pub fn storage_key(&self) -> String {
        format!(
            "p{}-{}-m{}-{}-g{}",
            self.project.len(),
            self.project,
            self.mission.len(),
            self.mission,
            self.generation
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentSessionState {
    Opening,
    Ready,
    Closing,
    Closed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSessionRecord {
    pub id: EnvironmentSessionId,
    pub plugin_id: String,
    pub plugin_version: String,
    pub plugin_digest: String,
    pub state: EnvironmentSessionState,
    #[serde(default)]
    pub source_shas: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_lock: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    pub created: u64,
    pub updated: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Store {
    pub fn plugin_runtime_dir(&self, plugin_id: &str) -> Result<std::path::PathBuf> {
        validate_component("plugin id", plugin_id)?;
        let data = self
            .root()
            .parent()
            .ok_or_else(|| Error::Store("store root has no data-root parent".into()))?;
        Ok(data.join("var/plugins").join(plugin_id))
    }

    pub fn environment_session_path(
        &self,
        plugin_id: &str,
        id: &EnvironmentSessionId,
    ) -> Result<std::path::PathBuf> {
        crate::validate_slug(&id.project)?;
        crate::validate_slug(&id.mission)?;
        Ok(self
            .plugin_runtime_dir(plugin_id)?
            .join("sessions")
            .join(format!("{}.json", id.storage_key())))
    }

    pub fn save_environment_session(&self, record: &EnvironmentSessionRecord) -> Result<()> {
        let path = self.environment_session_path(&record.plugin_id, &record.id)?;
        std::fs::create_dir_all(path.parent().expect("session path has parent"))?;
        let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn load_environment_session(
        &self,
        plugin_id: &str,
        id: &EnvironmentSessionId,
    ) -> Result<EnvironmentSessionRecord> {
        let path = self.environment_session_path(plugin_id, id)?;
        serde_json::from_slice(&std::fs::read(&path)?).map_err(Into::into)
    }

    pub fn load_environment_session_key(
        &self,
        plugin_id: &str,
        key: &str,
    ) -> Result<EnvironmentSessionRecord> {
        validate_component("environment session key", key)?;
        let path = self
            .plugin_runtime_dir(plugin_id)?
            .join("sessions")
            .join(format!("{key}.json"));
        serde_json::from_slice(&std::fs::read(&path)?).map_err(Into::into)
    }
}

fn validate_component(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    {
        return Err(Error::Store(format!("invalid {label} {value:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_record_round_trips_outside_the_project_tree() {
        let root = std::env::temp_dir().join(format!(
            "corpus-environment-record-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::new(root.join("store"));
        let id = EnvironmentSessionId {
            project: "alpha".into(),
            mission: "quote-race".into(),
            generation: 3,
        };
        let now = 1_787_112_000;
        let record = EnvironmentSessionRecord {
            id: id.clone(),
            plugin_id: "nutshell-regtest".into(),
            plugin_version: "1.0.0".into(),
            plugin_digest: "sha256:fixture".into(),
            state: EnvironmentSessionState::Opening,
            source_shas: BTreeMap::from([("nutshell".into(), "a".repeat(40))]),
            environment_lock: Some("lock:fixture".into()),
            image_digest: None,
            created: now,
            updated: now,
            error: None,
        };
        store.save_environment_session(&record).unwrap();
        assert_eq!(
            store.load_environment_session("nutshell-regtest", &id).unwrap(),
            record
        );
        let path = store
            .environment_session_path("nutshell-regtest", &id)
            .unwrap();
        assert!(!path.starts_with(store.project_dir("alpha")));
        let _ = std::fs::remove_dir_all(root);
    }
}
