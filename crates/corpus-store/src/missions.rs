//! Mission records, lifecycle guards, and curator dispatch transitions.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::environment::EnvironmentSessionState;
use crate::error::{Error, Result};
use crate::filesystem::atomic_write;
use crate::frontmatter;
use crate::projects::Project;
use crate::store::{validate_slug, Store};
use crate::yaml;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MissionRunRef {
    pub project: String,
    pub mission: String,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionControl {
    pub run_id: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionLaunchRequest {
    pub requested_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<MissionRunRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionDeleteRequest {
    pub requested_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MissionCompletion {
    Completed {
        at: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<String>,
    },
    LaunchFailed {
        at: u64,
        error: String,
    },
    UnexpectedExit {
        at: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionDispatchAbandonment {
    pub message_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionDispatch {
    pub parent: MissionRunRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
    #[serde(default)]
    pub live_seen: bool,
    #[serde(default)]
    pub running_seen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<MissionCompletion>,
    #[serde(default)]
    pub delivery_attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_message_id: Option<String>,
    #[serde(default)]
    pub delivered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_abandoned: Option<MissionDispatchAbandonment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionDispatchIdentity {
    pub parent: MissionRunRef,
    pub child_run_id: Option<String>,
    pub completion: MissionCompletion,
}

impl<'de> Deserialize<'de> for MissionLaunchRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Legacy(u64),
            Current {
                requested_at: u64,
                #[serde(default)]
                requested_by: Option<MissionRunRef>,
            },
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Legacy(requested_at) => Self {
                requested_at,
                requested_by: None,
            },
            Wire::Current {
                requested_at,
                requested_by,
            } => Self {
                requested_at,
                requested_by,
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub agent: String,
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    #[serde(default)]
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<MissionControl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_session: Option<String>,
    /// Relocatable id of the exact pin-specific working directory that owns
    /// `opencode_session`. It is deliberately not an absolute path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_requested: Option<MissionLaunchRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_requested: Option<MissionDeleteRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<MissionDispatch>,
}

impl Mission {
    pub fn load(store: &Store, project: &str, slug: &str) -> Result<Self> {
        let path = store
            .project_missions_dir(project)
            .join(format!("{slug}.md"));
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("mission not found: {project}/{slug}")))?;
        Self::parse(&raw)
    }

    fn parse(raw: &str) -> Result<Self> {
        let (frontmatter, _) = frontmatter::split(raw)?;
        let frontmatter =
            frontmatter.ok_or_else(|| Error::Store("mission has no frontmatter".into()))?;
        let mission: Self = yaml::from_value(yaml::Value::Mapping(frontmatter))
            .map_err(|error| Error::Store(format!("mission: {error}")))?;
        if mission.agent.is_empty() {
            return Err(Error::Store("mission missing agent ref".into()));
        }
        Ok(mission)
    }

    fn save(&self, store: &Store, project: &str, slug: &str, brief: &str) -> Result<()> {
        let dir = store.project_missions_dir(project);
        fs::create_dir_all(&dir)?;
        let frontmatter = yaml::to_string(self)?;
        atomic_write(
            &dir.join(format!("{slug}.md")),
            format!("---\n{frontmatter}---\n\n{brief}"),
        )
    }
}

impl Store {
    pub fn write_mission(
        &self,
        project: &str,
        slug: &str,
        mission: &Mission,
        brief: &str,
    ) -> Result<()> {
        validate_slug(slug)?;
        let project_record = Project::load(self, project)
            .map_err(|_| Error::Store(format!("project not found: {project}")))?;
        if project_record.delete_requested.is_some() {
            return Err(Error::Store(format!(
                "project {project} is pending deletion"
            )));
        }
        let agent = self.load_agent(project, &mission.agent).map_err(|error| {
            Error::Store(format!(
                "mission {slug}: agent {:?}: {error}",
                mission.agent
            ))
        })?;
        if agent.meta.delete_requested.is_some() {
            return Err(Error::Store(format!(
                "agent {project}/{} is pending deletion",
                mission.agent
            )));
        }
        mission.save(self, project, slug, brief)
    }

    pub fn list_missions(&self, project: &str) -> Result<Vec<(String, Mission)>> {
        let mut found = Vec::new();
        let dir = self.project_missions_dir(project);
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("md")
            {
                if let Some(slug) = path.file_stem().and_then(|stem| stem.to_str()) {
                    if let Ok(mission) = Mission::load(self, project, slug) {
                        found.push((slug.to_string(), mission));
                    }
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    pub fn load_mission(&self, project: &str, slug: &str) -> Result<Mission> {
        Mission::load(self, project, slug)
    }

    pub fn mission_brief(&self, project: &str, slug: &str) -> Result<String> {
        let path = self
            .project_missions_dir(project)
            .join(format!("{slug}.md"));
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("mission not found: {project}/{slug}")))?;
        let (frontmatter, body) = frontmatter::split(&raw)?;
        if frontmatter.is_none() {
            return Err(Error::Store("mission has no frontmatter".into()));
        }
        Ok(body.to_string())
    }

    pub fn delete_mission(&self, project: &str, slug: &str) -> Result<()> {
        self.ensure_mission_deletable(project, slug)?;
        self.delete_mission_record(project, slug)
    }

    pub fn ensure_mission_deletable(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let mission = self.load_mission(project, slug)?;
        if mission.session.is_some() {
            return Err(Error::Store(format!(
                "mission {project}/{slug} still has a run session; request lifecycle teardown first"
            )));
        }
        if let Some(key) = mission.environment_session.as_deref() {
            let plugin = Project::load(self, project)?.plugin;
            let record = self
                .load_environment_session_key(&plugin, key)
                .map_err(|error| {
                    Error::Store(format!(
                        "mission {project}/{slug} still has environment cleanup identity {key}: {error}"
                    ))
                })?;
            if record.state != EnvironmentSessionState::Closed {
                return Err(Error::Store(format!(
                    "mission {project}/{slug} environment is {}; request lifecycle teardown first",
                    format!("{:?}", record.state).to_ascii_lowercase()
                )));
            }
        }
        Ok(())
    }

    fn delete_mission_record(&self, project: &str, slug: &str) -> Result<()> {
        let path = self
            .project_missions_dir(project)
            .join(format!("{slug}.md"));
        if !path.is_file() {
            return Err(Error::Store(format!("mission not found: {project}/{slug}")));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn update_mission(&self, project: &str, slug: &str, mission: &Mission) -> Result<()> {
        let brief = self.mission_brief(project, slug)?;
        mission.save(self, project, slug, &brief)
    }

    pub fn consume_mission_launch_request(
        &self,
        project: &str,
        slug: &str,
        bind_origin: bool,
    ) -> Result<Option<MissionLaunchRequest>> {
        let mut mission = self.load_mission(project, slug)?;
        let request = mission.launch_requested.take();
        if bind_origin {
            mission.dispatch = request
                .as_ref()
                .and_then(|request| request.requested_by.clone())
                .map(|parent| MissionDispatch {
                    parent,
                    child_run_id: None,
                    live_seen: false,
                    running_seen: false,
                    completion: None,
                    delivery_attempt: 0,
                    delivery_message_id: None,
                    delivered: false,
                    delivery_abandoned: None,
                });
        }
        self.update_mission(project, slug, &mission)?;
        Ok(request)
    }

    pub fn record_mission_dispatch_completion(
        &self,
        project: &str,
        slug: &str,
        completion: MissionCompletion,
    ) -> Result<bool> {
        let mut mission = self.load_mission(project, slug)?;
        let Some(dispatch) = mission.dispatch.as_mut() else {
            return Ok(false);
        };
        if dispatch.completion.is_some() {
            return Ok(false);
        }
        dispatch.completion = Some(completion);
        self.update_mission(project, slug, &mission)?;
        Ok(true)
    }

    pub fn admit_mission_dispatch_delivery(
        &self,
        project: &str,
        slug: &str,
        identity: &MissionDispatchIdentity,
        attempt: u32,
        message_id: &str,
    ) -> Result<bool> {
        self.update_dispatch_if(project, slug, identity, |dispatch| {
            if dispatch.delivery_message_id.is_some() {
                return false;
            }
            dispatch.delivery_attempt = attempt;
            dispatch.delivery_message_id = Some(message_id.to_string());
            dispatch.delivered = false;
            dispatch.delivery_abandoned = None;
            true
        })
    }

    pub fn acknowledge_mission_dispatch_delivery(
        &self,
        project: &str,
        slug: &str,
        identity: &MissionDispatchIdentity,
        message_id: &str,
    ) -> Result<bool> {
        self.update_dispatch_if(project, slug, identity, |dispatch| {
            if dispatch.delivery_message_id.as_deref() != Some(message_id)
                || dispatch.delivered
                || dispatch.delivery_abandoned.is_some()
            {
                return false;
            }
            dispatch.delivered = true;
            true
        })
    }

    pub fn abandon_mission_dispatch_delivery(
        &self,
        project: &str,
        slug: &str,
        identity: &MissionDispatchIdentity,
        message_id: &str,
    ) -> Result<bool> {
        self.update_dispatch_if(project, slug, identity, |dispatch| {
            if dispatch.delivery_message_id.as_deref() != Some(message_id)
                || dispatch.delivered
                || dispatch.delivery_abandoned.is_some()
            {
                return false;
            }
            dispatch.delivery_abandoned = Some(MissionDispatchAbandonment {
                message_id: message_id.to_string(),
                reason: "interrupted".to_string(),
            });
            true
        })
    }

    pub fn retry_mission_dispatch_delivery(
        &self,
        project: &str,
        slug: &str,
        identity: &MissionDispatchIdentity,
        message_id: &str,
    ) -> Result<bool> {
        self.update_dispatch_if(project, slug, identity, |dispatch| {
            if dispatch.delivery_message_id.as_deref() != Some(message_id)
                || dispatch.delivered
                || dispatch.delivery_abandoned.is_some()
            {
                return false;
            }
            dispatch.delivery_message_id = None;
            true
        })
    }

    fn update_dispatch_if(
        &self,
        project: &str,
        slug: &str,
        identity: &MissionDispatchIdentity,
        update: impl FnOnce(&mut MissionDispatch) -> bool,
    ) -> Result<bool> {
        let mut mission = self.load_mission(project, slug)?;
        let Some(dispatch) = mission.dispatch.as_mut() else {
            return Ok(false);
        };
        if dispatch.parent != identity.parent
            || dispatch.child_run_id != identity.child_run_id
            || dispatch.completion.as_ref() != Some(&identity.completion)
            || !update(dispatch)
        {
            return Ok(false);
        }
        self.update_mission(project, slug, &mission)?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "missions/tests.rs"]
mod tests;
