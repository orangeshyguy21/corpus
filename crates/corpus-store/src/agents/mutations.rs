//! Validated in-place mutations for stored agent documents and role metadata.

use super::repository::{read_sidecar, stamp_and_write_sidecar, write_agent_doc};
use super::validation::validate_agent_doc;
use super::{infer_role, AddSubagentRequest, AgentRole, RoleMigration};
use crate::error::{Error, Result};
use crate::store::{validate_slug, Store};

impl Store {
    /// Edit one non-structural field of one agent-map entry.
    pub fn set_agent_field(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        field: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        if matches!(field, "mode" | "permission") {
            return Err(Error::Store(format!(
                "{field:?} is not settable here: use set_agent_role / set_agent_permission \
                 (mode is fixed by the entry's position in the document)"
            )));
        }
        let mut config = self.load_agent(project, slug)?;
        let target = entry.unwrap_or(slug);
        let entry = entry_mut(&mut config.doc, project, slug, target)?;
        if value.is_null() {
            entry.remove(field);
        } else {
            entry.insert(field.to_string(), value);
        }
        validate_and_write(self, project, slug, &config.doc)
    }

    /// Merge a top-level permission patch; null removes one permission family.
    pub fn patch_agent_permission(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        patch: &serde_json::Value,
    ) -> Result<()> {
        let patch = patch
            .as_object()
            .ok_or_else(|| Error::Store("permission patch must be an object".into()))?;
        let mut config = self.load_agent(project, slug)?;
        let target = entry.unwrap_or(slug);
        let entry = entry_mut(&mut config.doc, project, slug, target)?;
        let mut block = entry
            .get("permission")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, value) in patch {
            if value.is_null() {
                block.remove(key);
            } else {
                block.insert(key.clone(), value.clone());
            }
        }
        entry.insert("permission".into(), serde_json::Value::Object(block));
        validate_and_write(self, project, slug, &config.doc)
    }

    /// Add a subagent and its exact delegation grant as one mutation.
    pub fn add_subagent(&self, request: &AddSubagentRequest) -> Result<()> {
        let AddSubagentRequest {
            project,
            agent: slug,
            name,
            description,
            prompt,
            model,
            role,
        } = request;
        validate_slug(name)?;
        if name == slug {
            return Err(Error::Store(format!(
                "subagent {name:?} would collide with its primary"
            )));
        }
        let mut config = self.load_agent(project, slug)?;
        if let Some(role) = role {
            ensure_role_compatible(project, slug, config.meta.role(), *role)?;
        }
        let original_doc = config.doc.clone();
        let agents = agent_map_mut(&mut config.doc, project, slug)?;
        if agents.contains_key(name) {
            return Err(Error::Store(format!(
                "agent {project}/{slug} already has an entry named {name:?}"
            )));
        }
        let mut entry = serde_json::Map::new();
        entry.insert("description".into(), description.as_str().into());
        entry.insert("mode".into(), "subagent".into());
        if !prompt.is_empty() {
            entry.insert("prompt".into(), prompt.as_str().into());
        }
        if let Some(model) = model.as_deref() {
            entry.insert("model".into(), model.into());
        }
        agents.insert(name.to_string(), serde_json::Value::Object(entry));
        allow_delegation(agents, slug, name);
        validate_and_write(self, project, slug, &config.doc)?;

        if let Some(role) = role {
            config.meta.subagent_roles.insert(name.to_string(), *role);
            let dir = self.project_agent_dir(project, slug);
            if let Err(error) = stamp_and_write_sidecar(&dir, &mut config.meta, self.actor()) {
                let _ = write_agent_doc(&dir, &original_doc);
                return Err(error);
            }
        }
        Ok(())
    }

    /// Remove a subagent, its delegation grant, and its sidecar role.
    pub fn remove_subagent(&self, project: &str, slug: &str, name: &str) -> Result<()> {
        let mut config = self.load_agent(project, slug)?;
        let original_doc = config.doc.clone();
        let agents = agent_map_mut(&mut config.doc, project, slug)?;
        if agents.remove(name).is_none() {
            return Err(Error::Store(format!(
                "agent {project}/{slug} has no entry named {name:?}"
            )));
        }
        remove_delegation(agents, slug, name);
        validate_and_write(self, project, slug, &config.doc)?;

        if config.meta.subagent_roles.remove(name).is_some() {
            let dir = self.project_agent_dir(project, slug);
            if let Err(error) = stamp_and_write_sidecar(&dir, &mut config.meta, self.actor()) {
                let _ = write_agent_doc(&dir, &original_doc);
                return Err(error);
            }
        }
        Ok(())
    }

    /// Set a known subagent's role after checking the session-wide ceiling.
    pub fn set_subagent_role(
        &self,
        project: &str,
        slug: &str,
        subagent: &str,
        role: AgentRole,
    ) -> Result<()> {
        let config = self.load_agent(project, slug)?;
        let known = config
            .doc
            .get("agent")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|agents| agents.contains_key(subagent));
        if !known {
            return Err(Error::Store(format!(
                "agent {project}/{slug} has no entry named {subagent:?}"
            )));
        }
        ensure_role_compatible(project, slug, config.meta.role(), role)?;
        let dir = self.project_agent_dir(project, slug);
        let mut meta = read_sidecar(&dir, slug);
        meta.subagent_roles.insert(subagent.to_string(), role);
        stamp_and_write_sidecar(&dir, &mut meta, self.actor())
    }

    /// Infer and optionally persist roles for pre-role agent sidecars.
    pub fn migrate_agent_roles(&self, project: &str, apply: bool) -> Result<Vec<RoleMigration>> {
        let mut migrations = Vec::new();
        for (slug, config) in self.list_agents(project)? {
            let already = config.meta.has_role();
            let inferred = config
                .doc
                .get("agent")
                .and_then(serde_json::Value::as_object)
                .and_then(|agents| {
                    agents
                        .iter()
                        .find(|(name, entry)| {
                            **name == slug
                                || entry
                                    .get("mode")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("primary")
                                    == "primary"
                        })
                        .and_then(|(_, entry)| entry.as_object())
                        .map(infer_role)
                })
                .unwrap_or(AgentRole::Researcher);
            let silent = config
                .doc
                .get("agent")
                .and_then(serde_json::Value::as_object)
                .and_then(|agents| agents.get(&slug))
                .is_some_and(|entry| entry.get("permission").is_none());
            if apply && !already {
                self.set_agent_role(project, &slug, inferred)?;
            }
            migrations.push(RoleMigration {
                agent: slug,
                current: config.meta.role,
                inferred,
                applied: apply && !already,
                needs_review: silent && inferred == AgentRole::Super,
            });
        }
        Ok(migrations)
    }
}

fn agent_map_mut<'a>(
    document: &'a mut serde_json::Value,
    project: &str,
    slug: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>> {
    document
        .get_mut("agent")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| Error::Store(format!("agent {project}/{slug}: missing agent map")))
}

fn entry_mut<'a>(
    document: &'a mut serde_json::Value,
    project: &str,
    slug: &str,
    entry: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>> {
    agent_map_mut(document, project, slug)?
        .get_mut(entry)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            Error::Store(format!(
                "agent {project}/{slug} has no entry named {entry:?}"
            ))
        })
}

fn validate_and_write(
    store: &Store,
    project: &str,
    slug: &str,
    document: &serde_json::Value,
) -> Result<()> {
    let dir = store.project_agent_dir(project, slug);
    validate_agent_doc(document, &dir)
        .map_err(|error| Error::Store(format!("agent {slug}: {error}")))?;
    write_agent_doc(&dir, document)
}

fn allow_delegation(
    agents: &mut serde_json::Map<String, serde_json::Value>,
    primary: &str,
    subagent: &str,
) {
    let Some(primary) = agents
        .get_mut(primary)
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let mut permission = primary
        .get("permission")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut task = match permission.get("task") {
        Some(serde_json::Value::Object(existing)) => existing.clone(),
        Some(serde_json::Value::String(action)) => {
            let mut rules = serde_json::Map::new();
            rules.insert("*".into(), action.clone().into());
            rules
        }
        _ => {
            let mut rules = serde_json::Map::new();
            rules.insert("*".into(), "deny".into());
            rules
        }
    };
    task.insert(subagent.to_string(), "allow".into());
    permission.insert("task".into(), serde_json::Value::Object(task));
    primary.insert("permission".into(), serde_json::Value::Object(permission));
}

fn remove_delegation(
    agents: &mut serde_json::Map<String, serde_json::Value>,
    primary: &str,
    subagent: &str,
) {
    if let Some(task) = agents
        .get_mut(primary)
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|entry| entry.get_mut("permission"))
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|permission| permission.get_mut("task"))
        .and_then(serde_json::Value::as_object_mut)
    {
        task.remove(subagent);
    }
}

fn ensure_role_compatible(
    project: &str,
    slug: &str,
    primary: AgentRole,
    requested: AgentRole,
) -> Result<()> {
    let incompatible = (primary == AgentRole::Curator && requested != AgentRole::Curator)
        || (requested == AgentRole::Curator
            && !matches!(primary, AgentRole::Curator | AgentRole::Super));
    if incompatible {
        return Err(Error::Store(format!(
            "agent {project}/{slug} is a {} and cannot hold a {} subagent — one role is \
             enforced per session, taken from the primary, so this would render as {}",
            primary.as_str(),
            requested.as_str(),
            requested.cap_under(primary).as_str()
        )));
    }
    Ok(())
}
