//! Read-only host observation used by operator administration.
//!
//! This crate may inspect installed plugin manifests/revision caches, list
//! corpus tmux sessions, stat raw captures, and ask OpenCode for its model
//! catalog. It cannot execute a plugin, fetch source, launch or stop a run,
//! call an oracle, or touch a sandbox.

pub mod models;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use corpus_store::{Error, Mission, Store};

pub use models::{
    model_list, ollama_models, ollama_models_refresh, ModelEntry, ModelList, ModelOption,
    ModelProviderGroup, ModelRegistry,
};

pub const WORKING_WINDOW_SECS: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionActivity {
    Idle,
    Waiting,
    Working,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissionRunState {
    pub activity: MissionActivity,
    pub idle_secs: Option<u64>,
}

pub fn live_tui_sessions() -> Vec<String> {
    let Some(tmux) = resolve_tmux() else {
        return Vec::new();
    };
    let Ok(output) = Command::new(tmux)
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with("corpus-"))
        .map(str::to_string)
        .collect()
}

pub fn activity_from_idle(live: bool, idle_secs: Option<u64>) -> MissionActivity {
    if !live {
        return MissionActivity::Idle;
    }
    match idle_secs {
        Some(secs) if secs < WORKING_WINDOW_SECS => MissionActivity::Working,
        _ => MissionActivity::Waiting,
    }
}

pub fn mission_run_state(
    store: &Store,
    project: &str,
    mission: &Mission,
    live: &[String],
) -> MissionRunState {
    let is_live = mission
        .session
        .as_deref()
        .is_some_and(|session| live.iter().any(|candidate| candidate == session));
    let idle_secs = mission
        .session
        .as_deref()
        .filter(|_| is_live)
        .and_then(|session| session_raw_log(store, project, session))
        .and_then(|log| run_idle_secs(&log));
    MissionRunState {
        activity: activity_from_idle(is_live, idle_secs),
        idle_secs,
    }
}

pub fn session_raw_log(store: &Store, project: &str, session: &str) -> Option<PathBuf> {
    let stem = session.strip_prefix("corpus-")?;
    let (agent, stamp) = stem.rsplit_once('-')?;
    if agent.is_empty() {
        return None;
    }
    let stamp = stamp.parse::<u64>().ok()?;
    Some(
        store
            .project_corpus_dir(project)
            .join(corpus_store::RUNS)
            .join(format!("{stamp}-{agent}.raw")),
    )
}

pub fn run_idle_secs(log: &Path) -> Option<u64> {
    let modified = fs::metadata(log).ok()?.modified().ok()?;
    Some(
        modified
            .elapsed()
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    )
}

/// Installed plugin names from manifests only. No plugin process is started.
pub fn plugin_names() -> Result<Vec<String>, Error> {
    let root = corpus_store::paths::plugins_dir();
    let mut names = Vec::new();
    if !root.is_dir() {
        return Ok(names);
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path().join("plugin.toml");
        if !path.is_file() {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = toml::from_str::<toml::Value>(&raw) else {
            continue;
        };
        if let Some(name) = value.get("name").and_then(toml::Value::as_str) {
            names.push(name.to_string());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Validate an author-time pin from installed manifests and the disk rev
/// cache only. Fail-open when the plugin/source catalog cannot be resolved;
/// launch performs authoritative resolution later.
pub fn validate_pin(store: &Store, project: &str, source: &str, rev: &str) -> Result<(), Error> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err(Error::Store(format!("pin {source}: rev is empty")));
    }
    let project = corpus_store::Project::load(store, project)?;
    let Some(plugin_dir) = plugin_dir_by_name(&project.plugin)? else {
        return Ok(());
    };
    let config = match fs::read_to_string(plugin_dir.join("config.toml")) {
        Ok(raw) => match toml::from_str::<toml::Value>(&raw) {
            Ok(config) => config,
            Err(_) => return Ok(()),
        },
        Err(_) => return Ok(()),
    };
    let declared = config
        .get("sources")
        .and_then(toml::Value::as_table)
        .is_some_and(|table| table.contains_key(&format!("{source}_sha")));
    if !declared {
        return Ok(());
    }
    let Ok(resources) = corpus_store::paths::resources_for_plugins() else {
        return Ok(());
    };
    let manifest_tag = source_manifest_tag(&resources.join("sources.toml"), source);
    let cached = cached_revs(
        &resources
            .join("sources/.rev-cache")
            .join(format!("{source}.json")),
    );
    let accepted = manifest_tag.as_deref() == Some(rev)
        || cached.iter().any(|candidate| candidate == rev)
        || matches!(rev, "main" | "master")
        || is_commit_sha(rev);
    if accepted {
        Ok(())
    } else {
        let mut known = cached;
        if let Some(tag) = manifest_tag {
            known.push(tag);
        }
        known.sort();
        known.dedup();
        Err(Error::Store(format!(
            "pin {source}={rev:?} is not a known rev, tag, main/master, or a 40-hex commit sha — known: {}",
            known.join(", ")
        )))
    }
}

fn plugin_dir_by_name(name: &str) -> Result<Option<PathBuf>, Error> {
    let root = corpus_store::paths::plugins_dir();
    if !root.is_dir() {
        return Ok(None);
    }
    for entry in fs::read_dir(root)? {
        let dir = entry?.path();
        let manifest = dir.join("plugin.toml");
        let Ok(raw) = fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(value) = toml::from_str::<toml::Value>(&raw) else {
            continue;
        };
        if value.get("name").and_then(toml::Value::as_str) == Some(name) {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}

fn source_manifest_tag(path: &Path, source: &str) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let value = toml::from_str::<toml::Value>(&raw).ok()?;
    value
        .get("sources")?
        .get(source)?
        .get("tag")?
        .as_str()
        .map(str::to_string)
}

fn cached_revs(path: &Path) -> Vec<String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    value
        .get("refs")
        .and_then(serde_json::Value::as_object)
        .map(|refs| refs.keys().cloned().collect())
        .unwrap_or_default()
}

fn is_commit_sha(rev: &str) -> bool {
    rev.len() == 40
        && rev
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn resolve_opencode() -> Result<PathBuf, Error> {
    if let Some(found) = on_path("opencode") {
        return Ok(found);
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(home).join(".opencode/bin/opencode");
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    if let Some(resources) = corpus_store::paths::resource_root_opt() {
        let candidate = resources.join(".opencode/node_modules/.bin/opencode");
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::Store(
        "opencode binary not found — tried PATH, ~/.opencode/bin/opencode, .opencode/node_modules/.bin/opencode. Install it or put it on PATH."
            .into(),
    ))
}

fn resolve_tmux() -> Option<PathBuf> {
    static TMUX: OnceLock<Option<PathBuf>> = OnceLock::new();
    TMUX.get_or_init(|| {
        on_path("tmux").or_else(|| {
            [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/opt/local/bin",
                "/usr/bin",
            ]
            .iter()
            .map(|dir| PathBuf::from(dir).join("tmux"))
            .find(|candidate| is_executable(candidate))
        })
    })
    .clone()
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_rule_is_total() {
        assert_eq!(activity_from_idle(false, Some(0)), MissionActivity::Idle);
        assert_eq!(activity_from_idle(true, None), MissionActivity::Waiting);
        assert_eq!(activity_from_idle(true, Some(2)), MissionActivity::Working);
        assert_eq!(activity_from_idle(true, Some(3)), MissionActivity::Waiting);
    }

    #[test]
    fn commit_sha_shape_is_strict() {
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(!is_commit_sha(&"A".repeat(40)));
        assert!(!is_commit_sha(&"a".repeat(39)));
    }
}
