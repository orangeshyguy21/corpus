//! OpenCode agent document and prompt-reference validation.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// The primary entry from a validated-style OpenCode document.
pub(super) fn primary_agent_cfg(
    document: &serde_json::Value,
    project: &str,
    slug: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let agents = document
        .get("agent")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| Error::Store(format!("agent {project}/{slug}: missing \"agent\" map")))?;
    let primary = agents.iter().find(|(_, config)| {
        config
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("primary")
            == "primary"
    });
    let Some((_, config)) = primary else {
        return Err(Error::Store(format!(
            "agent {project}/{slug}: no primary agent in opencode.json"
        )));
    };
    Ok(config.as_object().cloned().unwrap_or_default())
}

pub(super) fn validate_agent_doc(document: &serde_json::Value, dir: &Path) -> Result<()> {
    let object = document.as_object().ok_or_else(|| {
        Error::Store(format!(
            "opencode.json must be a JSON object, got {} — pass the object itself, not its string serialization",
            json_kind(document)
        ))
    })?;
    let agents = object
        .get("agent")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| Error::Store("missing \"agent\" map".into()))?;
    if agents.is_empty() {
        return Err(Error::Store("agent map is empty".into()));
    }
    let primaries = agents
        .values()
        .filter(|config| {
            config
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("primary")
                == "primary"
        })
        .count();
    if primaries != 1 {
        return Err(Error::Store("exactly one primary agent is required".into()));
    }
    for (name, value) in agents {
        let config = value
            .as_object()
            .ok_or_else(|| Error::Store(format!("agent {name}: must be an object")))?;
        if let Some(permission) = config.get("permission") {
            validate_permission(permission)
                .map_err(|error| Error::Store(format!("agent {name}: {error}")))?;
        }
        if let Some(prompt) = config.get("prompt").and_then(serde_json::Value::as_str) {
            validate_prompt_refs(name, dir, prompt)?;
        }
    }
    Ok(())
}

fn validate_prompt_refs(name: &str, dir: &Path, prompt: &str) -> Result<()> {
    let mut rest = prompt;
    while let Some(start) = rest.find("{file:") {
        rest = &rest[start + 6..];
        let Some(end) = rest.find('}') else {
            return Err(Error::Store(format!(
                "agent {name}: unterminated {{file:}} ref"
            )));
        };
        let relative = &rest[..end];
        resolve_prompt_ref(dir, relative)
            .map_err(|error| Error::Store(format!("agent {name}: {error}")))?;
        rest = &rest[end + 1..];
    }
    Ok(())
}

/// Resolve a prompt reference to a canonical regular file confined beneath
/// the canonical agent directory. Renderers read the returned path directly,
/// so an in-directory symlink never becomes the path used for I/O.
pub(super) fn resolve_prompt_ref(dir: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative.is_empty()
        || relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::Store(format!(
            "{{file:{relative}}} must be a relative file inside the agent dir"
        )));
    }
    let root = fs::canonicalize(dir)
        .map_err(|error| Error::Store(format!("cannot resolve agent dir: {error}")))?;
    let candidate = fs::canonicalize(dir.join(relative_path)).map_err(|_| {
        Error::Store(format!(
            "{{file:{relative}}} does not resolve against the agent dir"
        ))
    })?;
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return Err(Error::Store(format!(
            "{{file:{relative}}} must resolve inside the agent dir"
        )));
    }
    Ok(candidate)
}

/// Permission values are actions or recursively nested rule maps.
fn validate_permission(permission: &serde_json::Value) -> Result<()> {
    let valid_action = |action: &str| ["ask", "allow", "deny"].contains(&action);
    match permission {
        serde_json::Value::String(action) if valid_action(action) => Ok(()),
        serde_json::Value::String(action) => Err(Error::Store(format!(
            "invalid permission action {action:?} (ask|allow|deny)"
        ))),
        serde_json::Value::Object(map) => {
            for value in map.values() {
                validate_permission(value)?;
            }
            Ok(())
        }
        _ => Err(Error::Store(
            "permission must be an action or a rule map".into(),
        )),
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}
