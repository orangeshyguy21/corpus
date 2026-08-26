//! Model, control credential, and agent launch identity policy.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;

use crate::error::{Error, Result};
use crate::models::ModelRegistry;
use crate::store::Store;

/// Resolve the effective launch model:
/// primary-agent model -> launch arg -> registry tool-use default -> refuse.
/// OpenCode's ambient default is never inherited.
pub(super) fn resolve_launch_model(
    store: &Store,
    project: &str,
    agent: &str,
    launch_model: Option<&str>,
) -> Result<String> {
    let config = store
        .load_agent(project, agent)
        .map_err(|e| Error::Store(format!("agent {project}/{agent}: {e}")))?;
    let primary_model = primary_agent_model(&config.doc);
    let model = pick_model(primary_model.as_deref(), launch_model)
        .or_else(registry_default)
        .ok_or_else(|| {
            Error::Store(format!(
                "no model configured for agent {agent} on {project} — set one on \
                 the primary agent entry, pass an explicit model, or register a \
                 tool-use model in benchmarks/models.yaml; opencode's ambient \
                 default is never inherited"
            ))
        })?;
    Ok(model)
}

/// The model a launch would pre-fill from the agent config (primary -> registry
/// tool-use default); None when neither is set.
pub fn agent_default_model(store: &Store, project: &str, agent: &str) -> Option<String> {
    let config = store.load_agent(project, agent).ok()?;
    primary_agent_model(&config.doc).or_else(registry_default)
}

/// The model declared on the primary agent's entry in the `agent` map.
fn primary_agent_model(doc: &serde_json::Value) -> Option<String> {
    let agents = doc.get("agent")?.as_object()?;
    for (_name, cfg) in agents {
        let mode = cfg
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("primary");
        if mode == "primary" {
            return cfg
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
    }
    None
}

/// The registry's tool-use default (the first tool-use entry, or the first
/// model). This IS an explicit model id; it replaces the old template
/// default (templates are gone).
fn registry_default() -> Option<String> {
    ModelRegistry::load_default().ok()?.launch_default()
}

/// First non-empty of two ordered options (primary -> arg).
fn pick_model(primary: Option<&str>, arg: Option<&str>) -> Option<String> {
    [primary, arg]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|m| !m.is_empty())
        .map(str::to_string)
}

pub(super) fn allocate_control_port() -> Result<u16> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        #[cfg(test)]
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            // The managed test sandbox forbids loopback listeners. Unit
            // tests launch only fake OpenCode binaries and never connect.
            return Ok(49_152 + (std::process::id() % 16_000) as u16);
        }
        Err(error) => {
            return Err(Error::Store(format!(
                "cannot allocate OpenCode control port: {error}"
            )))
        }
    };
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| Error::Store(format!("cannot inspect OpenCode control port: {error}")))
}

/// Stable secret for one app-launched OpenCode loopback server. It lives in
/// `<store parent>/var`, outside every project-visible run tree; another run
/// receives a different secret.
pub fn opencode_control_password(store: &Store, run_id: &str) -> Result<String> {
    let directory = store.var_dir().join("opencode-control");
    fs::create_dir_all(&directory)?;
    let path = directory.join(crate::store::fnv1a_hex(run_id.as_bytes()));
    if let Ok(existing) = fs::read_to_string(&path) {
        let existing = existing.trim();
        if existing.len() == 64 && existing.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(existing.to_string());
        }
        return Err(Error::Store(format!(
            "OpenCode control token is malformed: {}",
            path.display()
        )));
    }

    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        Error::Store(format!("cannot generate OpenCode control token: {error}"))
    })?;
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(token.as_bytes())?;
            file.write_all(b"\n")?;
            Ok(token)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read_to_string(&path)?;
            let existing = existing.trim();
            if existing.len() == 64 && existing.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Ok(existing.to_string())
            } else {
                Err(Error::Store(format!(
                    "OpenCode control token is malformed: {}",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// The materialized agent file stem: bare (slugified) — no team prefix.
pub fn agent_file_stem(agent: &str) -> String {
    crate::store::slugify(agent)
}

/// The opencode `--agent` handle for a launched agent: its project-unique,
/// name-derived identifier (see [`crate::primary_handles`]). This is what
/// opencode shows and resolves the rendered `.opencode/agent/<handle>.md`
/// by, so it must match what the renderer wrote. Falls back to the dir slug
/// when the project can't be listed — the same value the renderer uses for
/// an unnamed agent.
pub fn opencode_agent_handle(store: &Store, project: &str, slug: &str) -> String {
    store
        .list_agents(project)
        .ok()
        .and_then(|agents| crate::primary_handles(&agents).remove(slug))
        .unwrap_or_else(|| crate::store::slugify(slug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn agent_names_are_bare() {
        assert_eq!(agent_file_stem("operator"), "operator");
        assert_eq!(agent_file_stem("Flow Agent"), "flow-agent");
        assert_eq!(agent_file_stem("My Auditor"), "my-auditor");
    }

    #[test]
    fn launch_model_precedence_and_loud_failure() {
        assert_eq!(
            pick_model(Some("inst"), Some("arg")).as_deref(),
            Some("inst")
        );
        assert_eq!(pick_model(None, Some("arg")).as_deref(), Some("arg"));
        assert_eq!(pick_model(None, Some("  ")), None);
    }

    #[test]
    fn control_password_is_stable_private_and_outside_the_store() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "corpus-control-token-{}-{stamp}",
            std::process::id()
        ));
        let store = Store::new(root.join("store"));
        let first = opencode_control_password(&store, "run-a").unwrap();
        let second = opencode_control_password(&store, "run-a").unwrap();
        let other = opencode_control_password(&store, "run-b").unwrap();
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(first.len(), 64);
        let path = root
            .join("var/opencode-control")
            .join(crate::store::fnv1a_hex(b"run-a"));
        assert!(path.is_file());
        assert!(!path.starts_with(store.root()));
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(&root);
    }
}
