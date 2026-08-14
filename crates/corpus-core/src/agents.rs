//! Project-scoped agents (data-model plan v2).
//!
//! An agent is a directory `store/projects/<p>/agents/<slug>/` wrapping an
//! opencode config:
//!
//! ```text
//! agent.yaml    corpus metadata sidecar (name, created, cloned_from)
//! opencode.json THE config — a schema-valid opencode document: $schema +
//!               "agent" map (primary [+ subagents]); model/description/
//!               prompt/permission/temperature per entry
//! prompts/*.md optional prompt bodies resolved by `{file:}` refs
//! ```
//!
//! `opencode.json` is consumed by opencode as-is (unknown top-level keys are
//! rejected by opencode's schema, so the document stays clean); corpus
//! metadata lives in the `agent.yaml` sidecar, never in the JSON.
//!
//! The renderer materializes the launched agent into `.opencode/agent/<name>.md`
//! — one file per `agent` map entry, frontmatter carrying description/mode/
//! model/temperature/permission and a body of the prompt with `{file:}` refs
//! inlined from the agent dir. `.opencode/agent/` is corpus-managed: a launch
//! first clears the previous generated set, then renders the launched agent's
//! files, so subagent names stay bare and the primary's `task:` permission
//! keys match verbatim.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::{now_epoch, validate_slug, Store};

/// The two core seed agents every project starts with.
pub const CORE_SEEDS: [&str; 2] = ["operator", "researcher"];

/// The corpus metadata sidecar (`agent.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSidecar {
    pub name: String,
    #[serde(default)]
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
}

/// A loaded agent: the sidecar metadata plus the parsed opencode.json doc.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub meta: AgentSidecar,
    pub doc: serde_json::Value,
}

/// The opencode config schema reference the seed documents carry.
pub const OPENCODE_SCHEMA: &str = "https://opencode.ai/config.json";

impl Store {
    // -----------------------------------------------------------------
    // Seeds
    // -----------------------------------------------------------------

    /// Seed the core agent pair into a project (called by create_project).
    pub fn seed_core_agents(&self, project: &str) -> Result<()> {
        for slug in CORE_SEEDS {
            if !self.project_agent_dir(project, slug).join("opencode.json").is_file() {
                self.create_agent_from_seed(project, slug, slug)?;
            }
        }
        Ok(())
    }

    /// Create an agent directory from a core seed name (or blank when the
    /// seed is missing). The seed's opencode.json + prompts/ are copied and a
    /// fresh sidecar is written.
    pub fn create_agent_from_seed(&self, project: &str, slug: &str, seed: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent already exists: {project}/{slug}")));
        }
        fs::create_dir_all(&dir)?;
        let seed_dir = self.seed_agents_dir().join(seed);
        // The seed may be absent (temp stores, tests): fall back to blank.
        if seed_dir.join("opencode.json").is_file() {
            copy_tree(&seed_dir, &dir)?;
        } else {
            write_blank_opencode(&dir)?;
        }
        write_sidecar(&dir, slug, None)?;
        Ok(())
    }

    /// Create a blank agent directory (empty `agent` map + fresh sidecar).
    pub fn create_blank_agent(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent already exists: {project}/{slug}")));
        }
        fs::create_dir_all(&dir)?;
        write_blank_opencode(&dir)?;
        write_sidecar(&dir, slug, None)?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // CRUD
    // -----------------------------------------------------------------

    /// List a project's agents, sorted by slug.
    pub fn list_agents(&self, project: &str) -> Result<Vec<(String, AgentConfig)>> {
        let mut found = Vec::new();
        let dir = self.project_agents_dir(project);
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() || !path.join("opencode.json").is_file() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Store("non-utf8 agent dir".into()))?;
            if let Ok(agent) = self.load_agent(project, slug) {
                found.push((slug.to_string(), agent));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// Load an agent: sidecar + parsed opencode.json.
    pub fn load_agent(&self, project: &str, slug: &str) -> Result<AgentConfig> {
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let meta = read_sidecar(&dir, slug);
        let raw = fs::read_to_string(dir.join("opencode.json"))
            .map_err(|e| Error::Store(format!("agent {project}/{slug}: {e}")))?;
        let doc = serde_json::from_str(&raw)
            .map_err(|e| Error::Store(format!("agent {project}/{slug}: invalid opencode.json: {e}")))?;
        Ok(AgentConfig { meta, doc })
    }

    /// Validate and save an agent's opencode.json. The document must parse,
    /// hold a non-empty `agent` map with exactly one primary, valid
    /// permission blocks, and resolvable `{file:}` prompt refs. A rejected
    /// document is never written.
    pub fn save_agent(&self, project: &str, slug: &str, doc: &serde_json::Value) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        validate_agent_doc(doc, &dir).map_err(|e| Error::Store(format!("agent {slug}: {e}")))?;
        let pretty = serde_json::to_string_pretty(doc)?;
        fs::write(dir.join("opencode.json"), pretty)?;
        Ok(())
    }

    /// Clone an agent (opencode.json + prompts); the sidecar records the
    /// source, and the config hash is recomputed from the copy.
    pub fn clone_agent(&self, project: &str, from: &str, to: &str) -> Result<()> {
        validate_slug(to)?;
        let source = self.project_agent_dir(project, from);
        if !source.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{from}")));
        }
        let dest = self.project_agent_dir(project, to);
        if dest.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent already exists: {project}/{to}")));
        }
        fs::create_dir_all(&dest)?;
        copy_tree(&source, &dest)?;
        write_sidecar(&dest, to, Some(from))?;
        Ok(())
    }

    /// Delete an agent directory.
    pub fn delete_agent(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// The agent's config hash: FNV-1a over the opencode.json bytes, hex.
    /// Recorded in every run transcript as the agent's provenance.
    pub fn agent_config_hash(&self, project: &str, slug: &str) -> Result<String> {
        let path = self.project_agent_dir(project, slug).join("opencode.json");
        let bytes =
            fs::read(&path).map_err(|_| Error::Store(format!("agent not found: {project}/{slug}")))?;
        Ok(crate::store::fnv1a_hex(&bytes))
    }

    // -----------------------------------------------------------------
    // Renderer (.opencode/agent/)
    // -----------------------------------------------------------------

    /// The directory materialized agents land in: `.opencode/agent/` next to
    /// the store root (the repo root).
    pub fn opencode_agent_dir(&self) -> PathBuf {
        self.root()
            .parent()
            .map(|p| p.join(".opencode").join("agent"))
            .unwrap_or_else(|| self.root().to_path_buf())
    }

    /// Clear the previously generated agent set in `.opencode/agent/`
    /// (corpus-managed: the dir is regenerated per launch).
    pub fn clear_opencode_agents(&self) {
        let dir = self.opencode_agent_dir();
        if let Ok(read) = fs::read_dir(&dir) {
            for entry in read.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }

    /// Render the launched agent into `.opencode/agent/*.md` (bare names) —
    /// after clearing the previous generated set. Returns the written paths.
    pub fn render_agent(&self, project: &str, slug: &str) -> Result<Vec<PathBuf>> {
        let agent = self.load_agent(project, slug)?;
        let out_dir = self.opencode_agent_dir();
        fs::create_dir_all(&out_dir)?;
        let dir = self.project_agent_dir(project, slug);
        let agent_map = agent
            .doc
            .get("agent")
            .and_then(|v| v.as_object())
            .ok_or_else(|| Error::Store(format!("agent {slug}: missing agent map")))?;
        let mut names: Vec<&String> = agent_map.keys().collect();
        names.sort();
        let mut written = Vec::new();
        for name in names {
            let cfg = agent_map[name]
                .as_object()
                .ok_or_else(|| Error::Store(format!("agent {slug}/{name}: not an object")))?;
            let body = render_agent_file(cfg, &dir)?;
            let dest = out_dir.join(format!("{name}.md"));
            fs::write(&dest, body)?;
            written.push(dest);
        }
        Ok(written)
    }
}

/// Render one agent-map entry into the opencode agent-markdown body.
fn render_agent_file(cfg: &serde_json::Map<String, serde_json::Value>, dir: &Path) -> Result<String> {
    let mut out = String::with_capacity(256);
    out.push_str("---\n");
    let description = cfg.get("description").and_then(|v| v.as_str()).unwrap_or("");
    out.push_str("description: ");
    out.push_str(description);
    out.push('\n');
    let mode = cfg.get("mode").and_then(|v| v.as_str()).unwrap_or("primary");
    out.push_str("mode: ");
    out.push_str(mode);
    out.push('\n');
    if let Some(model) = cfg.get("model").and_then(|v| v.as_str()).filter(|m| !m.is_empty()) {
        out.push_str("model: ");
        out.push_str(model);
        out.push('\n');
    }
    if let Some(temperature) = cfg.get("temperature").and_then(|v| v.as_f64()) {
        out.push_str("temperature: ");
        out.push_str(&format!("{temperature}"));
        out.push('\n');
    }
    if let Some(permission) = cfg.get("permission") {
        out.push_str("permission:\n");
        let yaml = serde_yaml::to_string(permission)
            .map_err(|e| Error::Store(format!("cannot serialize permission: {e}")))?;
        for line in yaml.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("---\n");
    if let Some(prompt) = cfg.get("prompt").and_then(|v| v.as_str()) {
        out.push_str(&inline_file_refs(dir, prompt)?);
    }
    Ok(out)
}

/// Substitute every `{file:<rel>}` token in a prompt with the contents of the
/// referenced file under the agent dir.
fn inline_file_refs(dir: &Path, prompt: &str) -> Result<String> {
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(start) = rest.find("{file:") {
        out.push_str(&rest[..start]);
        rest = &rest[start + 6..];
        let Some(end) = rest.find('}') else {
            return Err(Error::Store("unterminated {file:} ref".into()));
        };
        let rel = &rest[..end];
        rest = &rest[end + 1..];
        let path = dir.join(rel);
        let body = fs::read_to_string(&path)
            .map_err(|e| Error::Store(format!("prompt ref {rel:?}: {e}")))?;
        out.push_str(&body);
    }
    out.push_str(rest);
    Ok(out)
}

/// Validate an agent opencode.json document. JSON already-parsed; checks the
/// structural rules the plan mandates before a save is allowed.
fn validate_agent_doc(doc: &serde_json::Value, dir: &Path) -> Result<()> {
    let obj = doc
        .as_object()
        .ok_or_else(|| Error::Store("opencode.json must be a JSON object".into()))?;
    let agents = obj
        .get("agent")
        .and_then(|v| v.as_object())
        .ok_or_else(|| Error::Store("missing \"agent\" map".into()))?;
    if agents.is_empty() {
        return Err(Error::Store("agent map is empty".into()));
    }
    let primaries = agents
        .values()
        .filter(|cfg| cfg.get("mode").and_then(|v| v.as_str()).unwrap_or("primary") == "primary")
        .count();
    if primaries != 1 {
        return Err(Error::Store("exactly one primary agent is required".into()));
    }
    for (name, cfg_value) in agents {
        let cfg = cfg_value
            .as_object()
            .ok_or_else(|| Error::Store(format!("agent {name}: must be an object")))?;
        if let Some(permission) = cfg.get("permission") {
            validate_permission(permission).map_err(|e| Error::Store(format!("agent {name}: {e}")))?;
        }
        if let Some(prompt) = cfg.get("prompt").and_then(|v| v.as_str()) {
            // Every {file:} ref must resolve against the agent dir.
            let mut rest = prompt;
            while let Some(start) = rest.find("{file:") {
                rest = &rest[start + 6..];
                let Some(end) = rest.find('}') else {
                    return Err(Error::Store(format!("agent {name}: unterminated {{file:}} ref")));
                };
                let rel = &rest[..end];
                if !dir.join(rel).is_file() {
                    return Err(Error::Store(format!(
                        "agent {name}: {{file:{rel}}} does not resolve against the agent dir"
                    )));
                }
                rest = &rest[end + 1..];
            }
        }
    }
    Ok(())
}

/// A permission block is valid when it is either a plain scalar (ask/allow/
/// deny) or an object (rule map) whose values are all valid actions.
fn validate_permission(permission: &serde_json::Value) -> Result<()> {
    fn ok_action(action: &str) -> bool {
        ["ask", "allow", "deny"].contains(&action)
    }
    match permission {
        serde_json::Value::String(action) if ok_action(action) => Ok(()),
        serde_json::Value::String(action) => Err(Error::Store(format!(
            "invalid permission action {action:?} (ask|allow|deny)"
        ))),
        serde_json::Value::Object(map) => {
            for (_, value) in map {
                validate_permission(value)?;
            }
            Ok(())
        }
        _ => Err(Error::Store("permission must be an action or a rule map".into())),
    }
}

/// The blank opencode.json: a schema-valid config with an empty agent map.
fn write_blank_opencode(dir: &Path) -> Result<()> {
    let doc = serde_json::json!({
        "$schema": OPENCODE_SCHEMA,
        "agent": {}
    });
    fs::write(dir.join("opencode.json"), serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}

fn write_sidecar(dir: &Path, name: &str, cloned_from: Option<&str>) -> Result<()> {
    let sidecar = AgentSidecar {
        name: name.to_string(),
        created: now_epoch(),
        cloned_from: cloned_from.map(str::to_string),
    };
    fs::write(dir.join("agent.yaml"), serde_yaml::to_string(&sidecar)?)?;
    Ok(())
}

fn read_sidecar(dir: &Path, slug: &str) -> AgentSidecar {
    fs::read_to_string(dir.join("agent.yaml"))
        .ok()
        .and_then(|raw| serde_yaml::from_str(&raw).ok())
        .unwrap_or(AgentSidecar {
            name: slug.to_string(),
            created: 0,
            cloned_from: None,
        })
}

/// Recursively copy a directory tree (agent dirs, seed dirs).
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        fs::create_dir_all(dst)?;
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("corpus-agents-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    fn doc(agent: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "$schema": OPENCODE_SCHEMA, "agent": agent })
    }

    #[test]
    fn save_agent_refuses_invalid_and_persists_valid() {
        let store = tmp_store("save");
        // A seed-less store still seeds the core pair; create a blank agent
        // to save against (no seed needed for the blank path).
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store.create_blank_agent("p", "a").unwrap();

        // No agent map.
        assert!(store
            .save_agent("p", "a", &serde_json::json!({ "$schema": "x" }))
            .is_err());
        // Empty agent map.
        assert!(store.save_agent("p", "a", &doc(serde_json::json!({}))).is_err());
        // Exactly one primary.
        assert!(store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "x"},
                    "two": {"mode": "primary", "prompt": "y"},
                })),
            )
            .is_err());
        // Bad permission action.
        assert!(store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "x", "permission": "hax"},
                })),
            )
            .is_err());
        // Unresolved {file:} prompt ref.
        assert!(store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "see {file:nope.md}"},
                })),
            )
            .is_err());
        // A valid document persists and reloads.
        store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "hello"},
                    "two": {"mode": "subagent", "prompt": "hi", "permission": {"task": "allow"}},
                })),
            )
            .unwrap();
        let agent = store.load_agent("p", "a").unwrap();
        let map = agent.doc.get("agent").unwrap().as_object().unwrap();
        assert!(map.contains_key("one") && map.contains_key("two"));
        let _ = fs::remove_dir_all(store.root());
    }
}