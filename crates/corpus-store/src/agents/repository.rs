//! Filesystem persistence for stored agent documents and sidecars.
//!
//! This module is the single low-level boundary for agent JSON/YAML writes and
//! recursive tree copies. Policy, validation, and lifecycle decisions remain
//! with the calling agent service methods.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::validation::{primary_agent_cfg, validate_agent_doc};
use super::{
    infer_role, AgentConfig, AgentRole, AgentSidecar, CreateAgentRequest, OPENCODE_SCHEMA,
};
use crate::error::{Error, Result};
use crate::filesystem::atomic_write;
use crate::store::{now_epoch, validate_slug, Store};

impl Store {
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
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::Store("non-utf8 agent dir".into()))?;
            if let Ok(agent) = self.load_agent(project, slug) {
                found.push((slug.to_string(), agent));
            }
        }
        found.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(found)
    }

    /// Load an agent's fail-closed sidecar and parsed OpenCode document.
    pub fn load_agent(&self, project: &str, slug: &str) -> Result<AgentConfig> {
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let meta = read_sidecar(&dir, slug);
        let raw = fs::read_to_string(dir.join("opencode.json"))
            .map_err(|error| Error::Store(format!("agent {project}/{slug}: {error}")))?;
        let doc = serde_json::from_str(&raw).map_err(|error| {
            Error::Store(format!(
                "agent {project}/{slug}: invalid opencode.json: {error}"
            ))
        })?;
        Ok(AgentConfig { meta, doc })
    }

    /// Validate and atomically replace an existing agent document.
    pub fn save_agent(&self, project: &str, slug: &str, doc: &serde_json::Value) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent not found: {project}/{slug} — create it first with agent_new or agent_clone"
            )));
        }
        validate_agent_doc(doc, &dir)
            .map_err(|error| Error::Store(format!("agent {slug}: {error}")))?;
        write_agent_doc(&dir, doc)
    }

    /// FNV-1a hash of the stored OpenCode bytes used as run provenance.
    pub fn agent_config_hash(&self, project: &str, slug: &str) -> Result<String> {
        let path = self.project_agent_dir(project, slug).join("opencode.json");
        let bytes = fs::read(&path)
            .map_err(|_| Error::Store(format!("agent not found: {project}/{slug}")))?;
        Ok(crate::store::fnv1a_hex(&bytes))
    }

    /// Set the sidecar display name without changing the path identity.
    pub fn set_agent_name(&self, project: &str, slug: &str, name: &str) -> Result<()> {
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let mut meta = read_sidecar(&dir, slug);
        let name = name.trim();
        meta.name = if name.is_empty() {
            slug.to_string()
        } else {
            name.to_string()
        };
        stamp_and_write_sidecar(&dir, &mut meta, self.actor())
    }

    /// Persist the server-enforced role while preserving sidecar provenance.
    pub fn set_agent_role(&self, project: &str, slug: &str, role: AgentRole) -> Result<()> {
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let mut meta = read_sidecar(&dir, slug);
        meta.role = Some(role);
        stamp_and_write_sidecar(&dir, &mut meta, self.actor())
    }

    /// Persist an idempotent deletion request for the app reconciler.
    pub fn request_agent_delete(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let mut meta = read_sidecar(&dir, slug);
        meta.delete_requested
            .get_or_insert(crate::store::MissionDeleteRequest {
                requested_at: now_epoch(),
            });
        stamp_and_write_sidecar(&dir, &mut meta, self.actor())
    }

    /// Return mission identities owned by an agent.
    pub fn missions_for_agent(&self, project: &str, slug: &str) -> Result<Vec<String>> {
        validate_slug(slug)?;
        Ok(self
            .list_missions(project)?
            .into_iter()
            .filter_map(|(mission_slug, mission)| (mission.agent == slug).then_some(mission_slug))
            .collect())
    }

    /// Delete an agent only after preflighting its complete mission cascade.
    pub fn delete_agent(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let missions = self.missions_for_agent(project, slug)?;
        for mission in &missions {
            self.ensure_mission_deletable(project, mission)?;
        }
        for mission in missions {
            self.delete_mission(project, &mission)?;
        }
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// Create the minimal valid document for a decided role.
    pub fn create_agent_with_role(&self, project: &str, slug: &str, role: AgentRole) -> Result<()> {
        validate_slug(slug)?;
        ensure_project_writable(self, project)?;
        let dir = self.project_agent_dir(project, slug);
        let mut pending = PendingAgentDir::create(&dir, project, slug)?;
        let doc = serde_json::json!({
            "$schema": OPENCODE_SCHEMA,
            "agent": {
                slug: {
                    "description": role.default_description(),
                    "mode": "primary",
                    "prompt": role.default_prompt(),
                }
            }
        });
        validate_agent_doc(&doc, &dir)
            .map_err(|error| Error::Store(format!("agent {slug}: {error}")))?;
        write_agent_doc(&dir, &doc)?;
        write_sidecar(&dir, slug, None, role, BTreeMap::new(), self.actor())?;
        pending.commit();
        Ok(())
    }

    /// Create an agent from typed content, optionally inheriting one source.
    pub fn create_agent(&self, request: &CreateAgentRequest) -> Result<()> {
        let CreateAgentRequest {
            project,
            slug,
            description,
            prompt,
            model,
            from,
            role,
        } = request;
        validate_slug(slug)?;
        ensure_project_writable(self, project)?;
        let dir = self.project_agent_dir(project, slug);

        let (mut pending, mut cfg) = if let Some(from) = from.as_deref() {
            let source = self.project_agent_dir(project, from);
            if !source.join("opencode.json").is_file() {
                return Err(Error::Store(format!(
                    "agent not found: {project}/{from} — 'from' must name an existing agent in this project"
                )));
            }
            let source_config = self.load_agent(project, from)?;
            if source_config.meta.delete_requested.is_some() {
                return Err(Error::Store(format!(
                    "agent {project}/{from} is pending deletion"
                )));
            }
            validate_agent_doc(&source_config.doc, &source)
                .map_err(|error| Error::Store(format!("agent {project}/{from}: {error}")))?;
            let cfg = primary_agent_cfg(&source_config.doc, project, from)?;
            (PendingAgentDir::copy(&source, &dir, project, slug)?, cfg)
        } else {
            (
                PendingAgentDir::create(&dir, project, slug)?,
                serde_json::Map::new(),
            )
        };

        cfg.insert("description".into(), description.as_str().into());
        cfg.insert("mode".into(), "primary".into());
        if !prompt.is_empty() {
            cfg.insert("prompt".into(), prompt.as_str().into());
        }
        if let Some(model) = model.as_deref() {
            cfg.insert("model".into(), model.into());
        }
        let role = role.unwrap_or_else(|| {
            if from.is_some() {
                infer_role(&cfg)
            } else {
                AgentRole::Researcher
            }
        });
        let doc = serde_json::json!({
            "$schema": OPENCODE_SCHEMA,
            "agent": { slug: cfg },
        });
        validate_agent_doc(&doc, &dir)
            .map_err(|error| Error::Store(format!("agent {slug}: {error}")))?;
        write_agent_doc(&dir, &doc)?;
        write_sidecar(
            &dir,
            slug,
            from.as_deref(),
            role,
            BTreeMap::new(),
            self.actor(),
        )?;
        pending.commit();
        Ok(())
    }

    /// Clone an agent inside one project.
    pub fn clone_agent(&self, project: &str, from: &str, to: &str) -> Result<()> {
        ensure_project_writable(self, project)?;
        if !self
            .project_agent_dir(project, from)
            .join("opencode.json")
            .is_file()
        {
            return Err(Error::Store(format!(
                "agent not found: {project}/{from} — 'from' must name an existing agent in this project (see agent_list)"
            )));
        }
        copy_agent_between(self, project, from, project, to, from)
    }

    /// Copy an agent between projects, retaining its complete policy tree.
    pub fn copy_agent(
        &self,
        from_project: &str,
        from: &str,
        to_project: &str,
        to: &str,
    ) -> Result<()> {
        ensure_project_writable(self, to_project)?;
        if !self
            .project_agent_dir(from_project, from)
            .join("opencode.json")
            .is_file()
        {
            return Err(Error::Store(format!(
                "agent not found: {from_project}/{from} (see agent_list)"
            )));
        }
        let provenance = format!("{from_project}/{from}");
        copy_agent_between(self, from_project, from, to_project, to, &provenance)
    }
}

fn ensure_project_writable(store: &Store, project: &str) -> Result<()> {
    if crate::projects::Project::load(store, project)?
        .delete_requested
        .is_some()
    {
        return Err(Error::Store(format!(
            "project {project} is pending deletion"
        )));
    }
    Ok(())
}

fn copy_agent_between(
    store: &Store,
    from_project: &str,
    from: &str,
    to_project: &str,
    to: &str,
    provenance: &str,
) -> Result<()> {
    validate_slug(to)?;
    let source = store.project_agent_dir(from_project, from);
    let source_config = store.load_agent(from_project, from)?;
    if source_config.meta.delete_requested.is_some() {
        return Err(Error::Store(format!(
            "agent {from_project}/{from} is pending deletion"
        )));
    }
    validate_agent_doc(&source_config.doc, &source)
        .map_err(|error| Error::Store(format!("agent {from_project}/{from}: {error}")))?;
    let primary = primary_agent_cfg(&source_config.doc, from_project, from)?;
    let mut doc = source_config.doc;
    let agents = doc
        .get_mut("agent")
        .and_then(|value| value.as_object_mut())
        .ok_or_else(|| Error::Store(format!("agent {from_project}/{from}: missing agent map")))?;
    let old_primary: Vec<String> = agents
        .iter()
        .filter(|(_, config)| {
            config
                .get("mode")
                .and_then(|mode| mode.as_str())
                .unwrap_or("primary")
                == "primary"
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in old_primary {
        agents.remove(&name);
    }
    agents.insert(to.to_string(), serde_json::Value::Object(primary));

    let dest = store.project_agent_dir(to_project, to);
    let mut pending = PendingAgentDir::copy(&source, &dest, to_project, to)?;
    validate_agent_doc(&doc, &dest)
        .map_err(|error| Error::Store(format!("agent {to_project}/{to}: {error}")))?;
    write_agent_doc(&dest, &doc)?;
    write_sidecar(
        &dest,
        to,
        Some(provenance),
        source_config.meta.role(),
        source_config.meta.subagent_roles,
        store.actor(),
    )?;
    pending.commit();
    Ok(())
}

/// Removes an unpublished agent directory on every failure path.
struct PendingAgentDir {
    path: std::path::PathBuf,
    committed: bool,
}

impl PendingAgentDir {
    fn create(path: &Path, project: &str, slug: &str) -> Result<Self> {
        fs::create_dir(path).map_err(|error| {
            Error::Store(format!("cannot create agent {project}/{slug}: {error}"))
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            committed: false,
        })
    }

    fn copy(source: &Path, path: &Path, project: &str, slug: &str) -> Result<Self> {
        copy_tree(source, path).map_err(|error| {
            Error::Store(format!("cannot create agent {project}/{slug}: {error}"))
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            committed: false,
        })
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PendingAgentDir {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(super) fn write_agent_doc(dir: &Path, doc: &serde_json::Value) -> Result<()> {
    atomic_write(
        &dir.join("opencode.json"),
        serde_json::to_string_pretty(doc)?,
    )?;
    Ok(())
}

/// Write a new sidecar with an explicit role and mutation provenance.
pub(super) fn write_sidecar(
    dir: &Path,
    name: &str,
    cloned_from: Option<&str>,
    role: AgentRole,
    subagent_roles: BTreeMap<String, AgentRole>,
    actor: &str,
) -> Result<()> {
    let sidecar = AgentSidecar {
        name: name.to_string(),
        created: now_epoch(),
        cloned_from: cloned_from.map(str::to_string),
        role: Some(role),
        subagent_roles,
        modified: Some(now_epoch()),
        modified_by: Some(actor.to_string()),
        delete_requested: None,
    };
    atomic_write(&dir.join("agent.yaml"), crate::yaml::to_string(&sidecar)?)?;
    Ok(())
}

/// Persist an in-place sidecar mutation and stamp its provenance.
pub(super) fn stamp_and_write_sidecar(
    dir: &Path,
    meta: &mut AgentSidecar,
    actor: &str,
) -> Result<()> {
    meta.modified = Some(now_epoch());
    meta.modified_by = Some(actor.to_string());
    atomic_write(&dir.join("agent.yaml"), crate::yaml::to_string(meta)?)?;
    Ok(())
}

/// Read a sidecar fail-closed: damage or absence never invents a role.
pub(super) fn read_sidecar(dir: &Path, slug: &str) -> AgentSidecar {
    fs::read_to_string(dir.join("agent.yaml"))
        .ok()
        .and_then(|raw| crate::yaml::from_str(&raw).ok())
        .unwrap_or(AgentSidecar {
            name: slug.to_string(),
            created: 0,
            cloned_from: None,
            role: None,
            subagent_roles: BTreeMap::new(),
            modified: None,
            modified_by: None,
            delete_requested: None,
        })
}

/// Recursively copy an agent directory without following links or special
/// files. Preflight the complete source before creating any copied entries so
/// a late symlink cannot leave a partially imported tree behind.
pub(super) fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    validate_copy_tree(src)?;
    // Claim the destination atomically. Following or merging into a planted
    // destination symlink/directory would let copied bytes escape this agent.
    fs::create_dir(dst).map_err(|error| {
        Error::Store(format!(
            "cannot create agent copy destination {}: {error}",
            dst.display()
        ))
    })?;
    if let Err(error) = copy_tree_contents(src, dst) {
        let _ = fs::remove_dir_all(dst);
        return Err(error);
    }
    Ok(())
}

fn validate_copy_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Store(format!(
            "refusing to copy symlink in agent tree: {}",
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(Error::Store(format!(
            "agent tree is not a directory: {}",
            path.display()
        )));
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(Error::Store(format!(
                "refusing to copy symlink in agent tree: {}",
                entry.path().display()
            )));
        }
        if file_type.is_dir() {
            validate_copy_tree(&entry.path())?;
        } else if !file_type.is_file() {
            return Err(Error::Store(format!(
                "refusing to copy special file in agent tree: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn copy_tree_contents(src: &Path, dst: &Path) -> Result<()> {
    let source_metadata = fs::symlink_metadata(src)?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(Error::Store(format!(
            "agent tree changed during copy: {}",
            src.display()
        )));
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(Error::Store(format!(
                "refusing to copy symlink in agent tree: {}",
                from.display()
            )));
        }
        if file_type.is_dir() {
            fs::create_dir(&to)?;
            copy_tree_contents(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        } else {
            return Err(Error::Store(format!(
                "refusing to copy special file in agent tree: {}",
                from.display()
            )));
        }
    }
    Ok(())
}
