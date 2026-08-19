//! Durable v1 environment-session open/close orchestration.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::{
    EnvironmentSessionId, EnvironmentSessionRecord, EnvironmentSessionState, Error, Plugin,
    PluginManifestVersion, Result, Store,
};

pub fn open_environment_session(
    store: &Store,
    id: EnvironmentSessionId,
    source_shas: BTreeMap<String, String>,
) -> Result<Option<EnvironmentSessionRecord>> {
    let project = crate::Project::load(store, &id.project)?;
    let plugin_dir = crate::find_plugin(&project.plugin)?
        .ok_or_else(|| Error::Store(format!("plugin not found: {}", project.plugin)))?;
    if plugin_dir.manifest.manifest_version != PluginManifestVersion::V1
        || !plugin_dir
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability == "sessions")
    {
        return Ok(None);
    }
    let version = plugin_dir
        .manifest
        .version
        .clone()
        .ok_or_else(|| Error::Store("session plugin has no version".into()))?;
    let now = epoch();
    let mut record = EnvironmentSessionRecord {
        id: id.clone(),
        plugin_id: plugin_dir.manifest.name.clone(),
        plugin_version: version,
        plugin_digest: crate::plugin_bundle_digest(&plugin_dir.dir)?,
        state: EnvironmentSessionState::Opening,
        source_shas: source_shas.clone(),
        environment_lock: None,
        image_digest: None,
        created: now,
        updated: now,
        error: None,
    };
    store.save_environment_session(&record)?;

    let state_dir = store
        .plugin_runtime_dir(&record.plugin_id)?
        .join("state")
        .join(id.storage_key());
    std::fs::create_dir_all(&state_dir)?;
    let source_cache = store.source_cache_dir();
    let sources: Vec<Value> = plugin_dir
        .manifest
        .sources
        .iter()
        .filter_map(|source| {
            source_shas.get(&source.id).map(|sha| {
                json!({
                    "id": source.id,
                    "sha": sha,
                    "host_path": source_cache.join(&source.id).join(sha),
                    "mount": source.mount,
                })
            })
        })
        .collect();
    let mut params = json!({
        "plugin_dir": plugin_dir.dir,
        "state_dir": state_dir,
        "source_cache": source_cache,
        "project": id.project,
        "mission": id.mission,
        "run": id.generation,
        "session_id": id.storage_key(),
        "sources": sources,
    });
    let operation_key = format!("session_open:{}", id.storage_key());
    params["idempotency_key"] = Value::String(operation_key.clone());
    let opened = (|| {
        let mut plugin = Plugin::spawn(&plugin_dir.dir)?;
        plugin.hello()?;
        let status = plugin.operation_status_with_params(
            &operation_key,
            params.as_object().cloned().unwrap_or_default(),
        )?;
        match status.state {
            crate::OperationState::Succeeded => Ok(status.result.unwrap_or(Value::Null)),
            crate::OperationState::Running => {
                let probe = plugin.session_probe_v1(&id.storage_key())?;
                if probe.get("ready").and_then(Value::as_bool).unwrap_or(false) {
                    Ok(probe)
                } else {
                    Err(Error::Plugin {
                        plugin: record.plugin_id.clone(),
                        message: "session_open is still running and the session is not ready"
                            .into(),
                    })
                }
            }
            crate::OperationState::Failed
                if !status.error.as_ref().is_some_and(|error| error.retryable) =>
            {
                Err(Error::Plugin {
                    plugin: record.plugin_id.clone(),
                    message: status
                        .error
                        .map(|error| format!("{}: {}", error.code, error.message))
                        .unwrap_or_else(|| "session_open previously failed".into()),
                })
            }
            crate::OperationState::Unknown | crate::OperationState::Failed => {
                plugin.call_v1("session_open", Some(params))
            }
        }
    })();
    match opened {
        Ok(result) => {
            record.state = EnvironmentSessionState::Ready;
            record.environment_lock = string_field(&result, "environment_lock");
            record.image_digest = string_field(&result, "image_digest");
            record.updated = epoch();
            store.save_environment_session(&record)?;
            Ok(Some(record))
        }
        Err(error) => {
            record.state = EnvironmentSessionState::Failed;
            record.error = Some(error.to_string());
            record.updated = epoch();
            store.save_environment_session(&record)?;
            Err(error)
        }
    }
}

pub fn close_environment_session(
    store: &Store,
    record: &mut EnvironmentSessionRecord,
) -> Result<()> {
    let plugin_dir = crate::find_plugin(&record.plugin_id)?
        .ok_or_else(|| Error::Store(format!("plugin not found: {}", record.plugin_id)))?;
    record.state = EnvironmentSessionState::Closing;
    record.updated = epoch();
    store.save_environment_session(record)?;
    let operation_key = format!("session_close:{}", record.id.storage_key());
    let mut params = json!({
        "session_id": record.id.storage_key(),
        "project": record.id.project,
        "mission": record.id.mission,
        "run": record.id.generation,
        "state_dir": store.plugin_runtime_dir(&record.plugin_id)?.join("state").join(record.id.storage_key()),
    });
    params["idempotency_key"] = Value::String(operation_key.clone());
    let closed = (|| {
        let mut plugin = Plugin::spawn(&plugin_dir.dir)?;
        plugin.hello()?;
        let status = plugin.operation_status_with_params(
            &operation_key,
            params.as_object().cloned().unwrap_or_default(),
        )?;
        match status.state {
            crate::OperationState::Succeeded => Ok(()),
            crate::OperationState::Running => Err(Error::Plugin {
                plugin: record.plugin_id.clone(),
                message: "session_close is already running".into(),
            }),
            crate::OperationState::Failed
                if !status.error.as_ref().is_some_and(|error| error.retryable) =>
            {
                Err(Error::Plugin {
                    plugin: record.plugin_id.clone(),
                    message: status
                        .error
                        .map(|error| format!("{}: {}", error.code, error.message))
                        .unwrap_or_else(|| "session_close previously failed".into()),
                })
            }
            crate::OperationState::Unknown | crate::OperationState::Failed => {
                plugin.call_v1("session_close", Some(params)).map(|_| ())
            }
        }
    })();
    match closed {
        Ok(()) => {
            record.state = EnvironmentSessionState::Closed;
            record.error = None;
            record.updated = epoch();
            store.save_environment_session(record)
        }
        Err(error) => {
            record.state = EnvironmentSessionState::Failed;
            record.error = Some(error.to_string());
            record.updated = epoch();
            store.save_environment_session(record)?;
            Err(error)
        }
    }
}

pub fn close_environment_session_key(store: &Store, plugin_id: &str, key: &str) -> Result<()> {
    let mut record = store.load_environment_session_key(plugin_id, key)?;
    close_environment_session(store, &mut record)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
