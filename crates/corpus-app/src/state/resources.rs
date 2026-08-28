//! Project, agent, mission, selection, and source/environment coordination.

use std::collections::BTreeMap;
use std::time::Duration;

use corpus_core::{Error, FindingIndexCache, Mission, MissionDeleteRequest, Project};

use super::{
    new_uuid_id, AppJobOutput, AppState, DeleteMissionResult, DeleteProjectResult, EnvStatus,
    FindingDiscovery, FindingSnapshot, ModelDiscovery, ProjectScopeSnapshot, ProjectTree, RunPhase,
    StopMissionResult,
};
use crate::jobs::{JobKind, JobScope, StartOutcome};
use crate::nav::Screen;

impl AppState {
    /// Re-list the projects from the store (and rebuild the sidebar tree).
    /// Newest-created first — the tree's default-open project is the most
    /// recent (the selection fallback takes `projects.first()`).
    pub fn refresh(&mut self) {
        self.project_index_revision = self.project_index_revision.saturating_add(1);
        if self.jobs.is_some() {
            self.schedule_project_index();
            return;
        }
        self.projects = self.store.list_projects().unwrap_or_default();
        self.projects
            .sort_by_key(|entry| std::cmp::Reverse(entry.1.created));
        self.refresh_trees();
    }

    /// Start the latest requested full index if one is not already active.
    /// The job key deliberately omits the revision, so event bursts collapse
    /// to one active scan and at most one follow-up scan.
    pub(super) fn schedule_project_index(&mut self) {
        let revision = self.project_index_revision;
        let Some(jobs) = self.jobs.as_mut() else {
            return;
        };
        let store = self.store.clone();
        let outcome = jobs.start(
            JobKind::ProjectIndex,
            JobScope {
                project: String::new(),
                project_generation: 0,
                corpus_revision: None,
                run_id: None,
            },
            Duration::from_secs(30),
            move |_| {
                let mut projects = store.list_projects().map_err(|error| error.to_string())?;
                projects.sort_by_key(|entry| std::cmp::Reverse(entry.1.created));
                let trees = projects
                    .iter()
                    .map(|(slug, _)| {
                        let agents = store.list_agents(slug).map_err(|error| error.to_string())?;
                        let missions = sort_missions(
                            store
                                .list_missions(slug)
                                .map_err(|error| error.to_string())?,
                        );
                        Ok((slug.clone(), ProjectTree { agents, missions }))
                    })
                    .collect::<Result<BTreeMap<_, _>, String>>()?;
                Ok(AppJobOutput::ProjectIndex {
                    revision,
                    projects,
                    trees,
                })
            },
        );
        if matches!(outcome, StartOutcome::Started(_)) {
            self.project_index_active_revision = Some(revision);
        }
        // A duplicate leaves the incremented requested revision in place as
        // the dirty marker; completion of the active scan schedules the
        // follow-up.
    }

    pub(super) fn apply_project_index(
        &mut self,
        revision: u64,
        projects: Vec<(String, Project)>,
        trees: BTreeMap<String, ProjectTree>,
    ) -> bool {
        if revision != self.project_index_revision {
            return false;
        }
        self.projects = projects;
        self.trees = trees;
        true
    }

    /// Rebuild the sidebar tree: every project's agents + missions. One
    /// dir scan per project — called from the refresh paths, never per
    /// frame.
    pub fn refresh_trees(&mut self) {
        self.trees = self
            .projects
            .iter()
            .map(|(slug, _)| {
                let tree = ProjectTree {
                    agents: self.store.list_agents(slug).unwrap_or_default(),
                    missions: sort_missions(self.store.list_missions(slug).unwrap_or_default()),
                };
                (slug.clone(), tree)
            })
            .collect();
    }

    pub fn source_revisions_loading(&self, project: &str) -> bool {
        self.source_revs_loading && self.source_revs_project.as_deref() == Some(project)
    }

    /// Re-list a project's agents (and keep its tree subtree fresh).
    pub fn refresh_agents(&mut self, project: &str) {
        let scope = self.job_scope(project, None);
        if let Some(jobs) = self.jobs.as_mut() {
            let store = self.store.clone();
            let project_owned = project.to_string();
            jobs.start(
                JobKind::ProjectAgents,
                scope,
                Duration::from_secs(15),
                move |_| {
                    store
                        .list_agents(&project_owned)
                        .map(AppJobOutput::Agents)
                        .map_err(|error| error.to_string())
                },
            );
            return;
        }
        self.agents = self.store.list_agents(project).unwrap_or_default();
        self.agents_project = Some(project.to_string());
        if let Some(tree) = self.trees.get_mut(project) {
            tree.agents = self.agents.clone();
        }
    }

    /// Re-list a project's missions, newest-created first (and keep its
    /// tree subtree fresh).
    pub fn refresh_missions(&mut self, project: &str) {
        let scope = self.job_scope(project, None);
        if let Some(jobs) = self.jobs.as_mut() {
            let store = self.store.clone();
            let project_owned = project.to_string();
            jobs.start(
                JobKind::ProjectMissions,
                scope,
                Duration::from_secs(15),
                move |_| {
                    store
                        .list_missions(&project_owned)
                        .map(sort_missions)
                        .map(AppJobOutput::Missions)
                        .map_err(|error| error.to_string())
                },
            );
            return;
        }
        self.missions = sort_missions(self.store.list_missions(project).unwrap_or_default());
        self.missions_project = Some(project.to_string());
        if let Some(tree) = self.trees.get_mut(project) {
            tree.missions = self.missions.clone();
        }
    }

    /// The sidebar's selected project — held by slug, falling back to the
    /// first project when unset or stale. `None` when there are no projects.
    pub fn effective_project(&self) -> Option<String> {
        self.selected_project
            .as_ref()
            .filter(|slug| {
                self.projects
                    .iter()
                    .any(|(s, project)| s == *slug && project.delete_requested.is_none())
            })
            .cloned()
            .or_else(|| {
                self.projects
                    .iter()
                    .find(|(_, project)| project.delete_requested.is_none())
                    .map(|(slug, _)| slug.clone())
            })
    }

    /// Select a project in the sidebar and (re)load its scoped caches —
    /// agents, missions, and the corpus summary all move to `slug`.
    pub fn select_project(&mut self, slug: &str) {
        let cached_and_selectable = self
            .projects
            .iter()
            .any(|(candidate, project)| candidate == slug && project.delete_requested.is_none());
        // This stat happens only for an explicit navigation action. It closes
        // the final ghost-row window without adding filesystem work to render
        // frames or ordinary selection maintenance.
        let exists = self.store.project_dir(slug).is_dir();
        if !cached_and_selectable || !exists {
            if !exists {
                self.prune_project_cache(slug);
                self.refresh();
            }
            return;
        }
        self.selected_project = Some(slug.to_string());
        if self.jobs.is_some() {
            let tree = self.trees.get(slug).cloned().unwrap_or_default();
            self.agents = tree.agents;
            self.missions = tree.missions;
            self.agents_project = Some(slug.to_string());
            self.missions_project = Some(slug.to_string());
        } else {
            self.refresh_agents(slug);
            self.refresh_missions(slug);
        }
        self.refresh_corpus_stats(slug);
        self.refresh_source_revs(slug);
        self.refresh_env(slug);
    }

    /// Select an agent and navigate to its editor. Like mission selection,
    /// this may switch project scope, but it performs no mutation itself.
    pub fn select_agent(&mut self, project: &str, slug: &str) {
        if self.effective_project().as_deref() != Some(project) {
            self.select_project(project);
        }
        self.selected_agent = Some(slug.to_string());
        self.current_screen = Screen::Agents;
    }

    /// Mission selection is navigation only. It may switch the cached project
    /// scope, but it never prepares, resumes, or spawns a run. Launch and
    /// Resume call their explicit state actions after selecting.
    pub fn select_mission(&mut self, project: &str, slug: &str) {
        if self.effective_project().as_deref() != Some(project) {
            self.select_project(project);
        }
        self.selected_mission = Some(slug.to_string());
        self.current_screen = Screen::Missions;
    }

    /// Load the source-rev dropdowns for the project's plugin (the plugin
    /// defines the revs AVAILABLE), seeding the selection from the
    /// PROJECT's stored pins (the project owns the pick) with any unset
    /// source at its default rev. When the plugin/sources can't be found
    /// the current pins are left untouched (the placeholder defaults
    /// hold) rather than cleared.
    pub fn refresh_source_revs(&mut self, project: &str) {
        let scope = self.job_scope(project, None);
        self.source_revs_project = Some(project.to_string());
        self.source_revs_loading = true;
        self.source_revs_error = None;
        let store = self.store.clone();
        let project_owned = project.to_string();
        let Some(jobs) = self.jobs.as_mut() else {
            let revs = corpus_core::plugin_sources(&store, &project_owned).unwrap_or_default();
            self.apply_source_revisions(project, revs);
            self.source_revs_loading = false;
            return;
        };
        jobs.start(
            JobKind::SourceRevisions,
            scope,
            Duration::from_secs(30),
            move |cancellation| {
                if cancellation.is_cancelled() {
                    return Err("source revision refresh cancelled".into());
                }
                corpus_core::plugin_sources(&store, &project_owned)
                    .map(AppJobOutput::SourceRevisions)
                    .map_err(|error| error.to_string())
            },
        );
    }

    /// The top-bar dropdown changed: update the in-memory selection and
    /// persist the pick onto the project (missions stamp it at creation).
    pub fn set_source_pin(&mut self, project: &str, repo: &str, rev: &str) -> Result<(), Error> {
        self.source_pins.insert(repo.to_string(), rev.to_string());
        let updated = self
            .store
            .set_project_pins(project, self.source_pins.clone())?;
        if let Some((_, spec)) = self.projects.iter_mut().find(|(s, _)| s == project) {
            *spec = updated;
        }
        Ok(())
    }

    /// The current env-status aggregation for a project's plugin: the
    /// probe's readiness and notes PLUS the version the target is actually
    /// running (from the live probe), so the top bar can show what is up
    /// and flag a source pin that disagrees.
    pub fn env_status(&self, project: &str) -> Option<EnvStatus> {
        let (_slug, spec) = self.projects.iter().find(|(slug, _)| slug == project)?;
        self.plugins
            .iter()
            .find(|p| p.name == spec.plugin)
            .map(|p| EnvStatus {
                name: p.name.clone(),
                ready: p.ready,
                notes: p.notes.clone(),
                running_version: p.running_version.clone(),
            })
    }

    /// Re-probe the env for a project (spawns the plugin's probe on the
    /// host — only ever on project switch or an explicit click, never
    /// per-frame).
    pub fn refresh_env(&mut self, project: &str) {
        self.env_project = Some(project.to_string());
        let plugin = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)
            .map(|(_, project)| project.plugin.clone());
        self.refresh_plugins(plugin.as_deref());
    }

    /// Make sure the selected project's caches (agents, missions, corpus
    /// summary) are loaded, and that `selected_project` is concrete (falls
    /// back to the first project). Called once a frame from `App::update` —
    /// stale checks are project-name equality, so this only hits disk on
    /// change.
    pub fn ensure_selection(&mut self) {
        let Some(first) = self
            .projects
            .iter()
            .find(|(_, project)| project.delete_requested.is_none())
            .map(|(slug, _)| slug.clone())
        else {
            self.selected_project = None;
            self.agents.clear();
            self.missions.clear();
            self.corpus_stats = None;
            self.corpus_cost = None;
            self.corpus_cost_cache = corpus_core::CorpusCostCache::default();
            self.finding_index_cache = FindingIndexCache::default();
            self.findings = FindingDiscovery::Loading;
            self.findings_project = None;
            self.corpus_revisions.clear();
            self.mission_logs.clear();
            self.agents_project = None;
            self.missions_project = None;
            self.corpus_stats_project = None;
            self.source_revs.clear();
            self.source_revs_project = None;
            self.source_revs_loading = false;
            self.source_revs_error = None;
            self.env_project = None;
            return;
        };
        let stale = !self.projects.iter().any(|(slug, project)| {
            project.delete_requested.is_none()
                && Some(slug.as_str()) == self.selected_project.as_deref()
        });
        if stale {
            self.selected_project = Some(first.clone());
        }
        let Some(project) = self.selected_project.clone() else {
            return;
        };
        if self.agents_project.as_deref() != Some(project.as_str()) {
            if self.jobs.is_some() {
                self.agents = self
                    .trees
                    .get(&project)
                    .map(|tree| tree.agents.clone())
                    .unwrap_or_default();
                self.agents_project = Some(project.clone());
            } else {
                self.refresh_agents(&project);
            }
        }
        if self.missions_project.as_deref() != Some(project.as_str()) {
            if self.jobs.is_some() {
                self.missions = self
                    .trees
                    .get(&project)
                    .map(|tree| tree.missions.clone())
                    .unwrap_or_default();
                self.missions_project = Some(project.clone());
            } else {
                self.refresh_missions(&project);
            }
        }
        if self.corpus_stats_project.as_deref() != Some(project.as_str()) {
            self.refresh_corpus_stats(&project);
        }
        if self.source_revs_project.as_deref() != Some(project.as_str()) {
            self.refresh_source_revs(&project);
        }
        if self.env_project.as_deref() != Some(project.as_str()) {
            self.refresh_env(&project);
        }
    }

    /// Create a project. The human gives the display name; the machine
    /// gives the id — an auto-generated UUIDv4, which is a valid
    /// kebab-case slug, so it slots straight into the store layout
    /// (`store/projects/<id>/`), CLI scopes, and `CORPUS_PROJECT`.
    pub fn create_project(&mut self, name: &str, plugin: &str) -> Result<(String, Project), Error> {
        // Human names mint human slugs ("Dep Scans" → "dep-scans"); only a
        // name with no alphanumerics falls back to the opaque id. (UUID
        // slugs made every chat/tool reference unreadable — 2026-08-14.)
        let slug = {
            let s = corpus_core::slugify(name);
            if s.is_empty() {
                new_uuid_id()
            } else {
                s
            }
        };
        let project = self.store.create_project(&slug, name, plugin)?;

        // Make the just-created project selectable before the asynchronous
        // project-index refresh completes. Otherwise `select_project` rejects
        // it as absent from the current cache and leaves the previous project
        // selected until a later interaction.
        self.projects.retain(|(candidate, _)| candidate != &slug);
        self.projects.push((slug.clone(), project.clone()));
        self.projects
            .sort_by_key(|entry| std::cmp::Reverse(entry.1.created));
        self.trees.entry(slug.clone()).or_default();
        self.select_project(&slug);
        self.current_screen = Screen::Projects;
        self.refresh();

        Ok((slug, project))
    }

    /// Clone a project; the copied name falls back to the source's when
    /// none is given. The slug derives from the name (kebab), else an id.
    pub fn clone_project(
        &self,
        from: &str,
        name: Option<&str>,
        with_corpus: bool,
    ) -> Result<(String, Project), Error> {
        let slug = name
            .map(corpus_core::slugify)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{from}-copy"));
        // A taken slug gets a numeric suffix rather than an opaque id.
        let slug = (2..)
            .map(|n| {
                if n == 2 {
                    slug.clone()
                } else {
                    format!("{slug}-{n}")
                }
            })
            .find(|s| !self.store.project_dir(s).exists())
            .unwrap_or_else(new_uuid_id);
        self.store
            .clone_project(from, &slug, name, with_corpus)
            .map(|p| (slug, p))
    }

    pub fn delete_project(&mut self, slug: &str) -> Result<DeleteProjectResult, Error> {
        let missions = self.store.list_missions(slug)?;
        if self.project_has_inflight_run(slug)
            || missions
                .iter()
                .any(|(mission, _)| self.store.ensure_mission_deletable(slug, mission).is_err())
        {
            self.store.request_project_delete(slug)?;
            let deleting = Project::load(&self.store, slug)?;
            if let Some((_, cached)) = self
                .projects
                .iter_mut()
                .find(|(project, _)| project == slug)
            {
                *cached = deleting;
            }
            if self.selected_project.as_deref() == Some(slug) {
                self.selected_project = None;
            }
            Ok(DeleteProjectResult::Scheduled)
        } else {
            self.store.delete_project(slug)?;
            self.prune_project_cache(slug);
            self.refresh();
            Ok(DeleteProjectResult::Completed)
        }
    }

    /// Remove one project from every render-facing cache in the same UI
    /// frame as durable deletion. Background reconciliation remains a safety
    /// net and cannot be the source of deletion responsiveness.
    pub(super) fn prune_project_cache(&mut self, slug: &str) {
        self.projects.retain(|(project, _)| project != slug);
        self.trees.remove(slug);
        self.corpus_revisions.remove(slug);
        if self.selected_project.as_deref() == Some(slug) {
            self.selected_project = None;
        }
        if self.agents_project.as_deref() == Some(slug) {
            self.agents.clear();
            self.agents_project = None;
            self.selected_agent = None;
        }
        if self.missions_project.as_deref() == Some(slug) {
            self.missions.clear();
            self.missions_project = None;
            self.selected_mission = None;
        }
        if self.corpus_stats_project.as_deref() == Some(slug) {
            self.corpus_stats = None;
            self.corpus_cost = None;
            self.corpus_cost_cache = corpus_core::CorpusCostCache::default();
            self.finding_index_cache = FindingIndexCache::default();
            self.findings = FindingDiscovery::Loading;
            self.findings_project = None;
            self.mission_logs.clear();
            self.corpus_stats_project = None;
            self.corpus_polled_at = None;
        }
        if self.source_revs_project.as_deref() == Some(slug) {
            self.source_pins.clear();
            self.source_revs.clear();
            self.source_revs_project = None;
            self.source_revs_loading = false;
            self.source_revs_error = None;
        }
        if self.env_project.as_deref() == Some(slug) {
            self.env_project = None;
        }
    }

    /// The app's remembered UI choices (`store/app.yaml`). Read on demand —
    /// it is a tiny file and the app touches it at launch and on a picker
    /// change, never per frame.
    pub fn prefs(&self) -> corpus_core::AppPrefs {
        self.store.load_prefs()
    }

    /// Remember the chat model the operator picked, so the next launch comes
    /// back on it. A write failure is deliberately swallowed: a read-only
    /// store must degrade to "this session only", not toast on every pick.
    pub fn remember_chat_model(&self, model: &str) {
        let mut prefs = self.store.load_prefs();
        if prefs.chat_model == model {
            return;
        }
        prefs.chat_model = model.to_string();
        let _ = self.store.save_prefs(&prefs);
    }

    /// Rename a project's display label (the slug — its identity in every
    /// path — is untouched).
    pub fn rename_project(&self, slug: &str, name: &str) -> Result<Project, Error> {
        self.store.rename_project(slug, name)
    }

    /// Change a project's environment plugin binding.
    pub fn rebind_project(&self, slug: &str, plugin: &str) -> Result<Project, Error> {
        self.store.rebind_project(slug, plugin)
    }

    /// Wipe a project's corpus (the Corpus panel's red Delete): categories
    /// are emptied and `corpus_generation` bumps; the project + agents
    /// survive. Returns the updated project.
    pub fn wipe_project_corpus(&mut self, slug: &str) -> Result<Project, Error> {
        let project = self.store.wipe_project_corpus(slug)?;
        if let Some((_, cached)) = self.projects.iter_mut().find(|(name, _)| name == slug) {
            *cached = project.clone();
        }
        self.note_corpus_mutation(slug);
        if self.findings_project.as_deref() == Some(slug) {
            self.finding_index_cache = FindingIndexCache::default();
            self.findings = FindingDiscovery::Ready(Vec::new());
        }
        Ok(project)
    }

    // --- agents ---

    /// Save (validate + write) an agent's opencode.json.
    pub fn save_agent(
        &self,
        project: &str,
        slug: &str,
        doc: &serde_json::Value,
    ) -> Result<(), Error> {
        self.store.save_agent(project, slug, doc)
    }

    // --- granular agent edits (the Forms tab) -------------------------
    // Each is a read-modify-validate-write in corpus-core, so the form
    // sends one value instead of rewriting the whole document.

    /// Set one field of an agent entry (`None` = the primary).
    pub fn set_agent_field(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        field: &str,
        value: serde_json::Value,
    ) -> Result<(), Error> {
        self.store
            .set_agent_field(project, slug, entry, field, value)
    }

    /// Rename an agent's display label (the slug — its identity in every
    /// path — is untouched).
    pub fn set_agent_name(&self, project: &str, slug: &str, name: &str) -> Result<(), Error> {
        self.store.set_agent_name(project, slug, name)
    }

    /// Set the agent's (or a subagent's) role — the server-enforced ceiling.
    pub fn set_agent_role(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        role: corpus_core::AgentRole,
    ) -> Result<(), Error> {
        match entry {
            Some(sub) => self.store.set_subagent_role(project, slug, sub, role),
            None => self.store.set_agent_role(project, slug, role),
        }
    }

    /// Merge a permission patch into an entry.
    pub fn patch_agent_permission(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        patch: &serde_json::Value,
    ) -> Result<(), Error> {
        self.store
            .patch_agent_permission(project, slug, entry, patch)
    }

    pub fn add_subagent(&self, request: corpus_core::AddSubagentRequest) -> Result<(), Error> {
        self.store.add_subagent(&request)
    }

    pub fn remove_subagent(&self, project: &str, slug: &str, name: &str) -> Result<(), Error> {
        self.store.remove_subagent(project, slug, name)
    }

    /// opencode's launchable model ids — the catalog an AGENT config must
    /// resolve against. Deliberately NOT `ollama_models()`, which is the
    /// chat's own (locally-pulled) list and would offer ids a mission
    /// cannot launch with. TTL-cached in corpus-core; `refresh` re-pulls.
    pub fn opencode_models(&mut self, refresh: bool) -> ModelDiscovery {
        if refresh || matches!(self.opencode_models, ModelDiscovery::Loading) {
            self.opencode_models = ModelDiscovery::Loading;
            let Some(jobs) = self.jobs.as_mut() else {
                self.opencode_models = match corpus_core::model_list(refresh) {
                    Ok(models) => ModelDiscovery::Ready(models),
                    Err(error) => ModelDiscovery::Failed(error.to_string()),
                };
                return self.opencode_models.clone();
            };
            jobs.start(
                JobKind::ModelDiscovery,
                JobScope {
                    project: String::new(),
                    project_generation: 0,
                    corpus_revision: None,
                    run_id: None,
                },
                Duration::from_secs(45),
                move |_| {
                    corpus_core::model_list(refresh)
                        .map(AppJobOutput::OpencodeModels)
                        .map_err(|error| error.to_string())
                },
            );
        }
        self.opencode_models.clone()
    }

    /// Clone an agent.
    pub fn clone_agent(&self, project: &str, from: &str) -> Result<(), Error> {
        let id = new_uuid_id();
        self.store.clone_agent(project, from, &id)
    }

    /// Delete an agent.
    pub fn delete_agent(&self, project: &str, slug: &str) -> Result<(), Error> {
        let missions = self.store.missions_for_agent(project, slug)?;
        if self.run_phases.iter().any(|(id, phase)| {
            id.project == project
                && missions.iter().any(|mission| mission == &id.mission)
                && phase.blocks_deletion()
        }) || missions.iter().any(|mission| {
            self.store
                .ensure_mission_deletable(project, mission)
                .is_err()
        }) {
            self.store.request_agent_delete(project, slug)
        } else {
            self.store.delete_agent(project, slug)
        }
    }

    /// Create a new (auto-id'd) agent from a ROLE — the sidebar's
    /// "+ agent" flow. Roles replaced the seed set: the role already
    /// decides the capability ceiling the renderer writes, so a seed
    /// document was only ever contributing a starting prompt, which now
    /// ships compiled into corpus-core.
    pub fn create_agent_with_role(
        &mut self,
        project: &str,
        role: corpus_core::AgentRole,
    ) -> Result<String, Error> {
        if Project::load(&self.store, project)?
            .delete_requested
            .is_some()
        {
            return Err(Error::Store("project deletion is pending".into()));
        }
        let id = new_uuid_id();
        self.store.create_agent_with_role(project, &id, role)?;
        // Stamp the human placeholder name so the Forms tab and the sidebar
        // show an editable label (and opencode a friendly handle), not the
        // opaque id. Best-effort: a naming failure must not undo a created
        // agent.
        let _ = self
            .store
            .set_agent_name(project, &id, corpus_core::DEFAULT_AGENT_NAME);

        // Make the newly created record renderable immediately. The normal
        // background refresh still reconciles the full list, but navigation
        // must not briefly treat this selection as stale and fall back to a
        // different agent while that refresh is in flight.
        if let Ok(agent) = self.store.load_agent(project, &id) {
            if let Some(tree) = self.trees.get_mut(project) {
                tree.agents.push((id.clone(), agent.clone()));
                tree.agents.sort_by(|(left, _), (right, _)| left.cmp(right));
            }
            if self.agents_project.as_deref() == Some(project) {
                self.agents.push((id.clone(), agent));
                self.agents.sort_by(|(left, _), (right, _)| left.cmp(right));
            }
        }
        Ok(id)
    }

    /// Create a mission record: auto-id slug, the agent ref, the current
    /// top-bar pins stamped in. Returns the mission slug.
    pub fn create_mission(&self, project: &str, agent: &str, brief: &str) -> Result<String, Error> {
        if Project::load(&self.store, project)?
            .delete_requested
            .is_some()
        {
            return Err(Error::Store("project deletion is pending".into()));
        }
        if self
            .store
            .load_agent(project, agent)?
            .meta
            .delete_requested
            .is_some()
        {
            return Err(Error::Store("agent deletion is pending".into()));
        }
        let id = new_uuid_id();
        let mission = Mission {
            agent: agent.to_string(),
            pins: self.source_pins.clone(),
            budget: None,
            created: self.clock.unix_seconds(),
            name: None,
            session: None,
            control: None,
            opencode_session: None,
            opencode_workspace: None,
            environment_session: None,
            launch_requested: None,
            delete_requested: None,
            dispatch: None,
        };
        self.store.write_mission(project, &id, &mission, brief)?;
        Ok(id)
    }

    /// Delete is the mission's single teardown verb. A live run is exported
    /// and cleaned up first; its mission record is removed only after that
    /// succeeds. Durable transcripts remain in `corpus/runs/`.
    pub fn delete_mission(
        &mut self,
        project: &str,
        slug: &str,
    ) -> Result<DeleteMissionResult, Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        // Persist intent before starting teardown. If the app exits after
        // killing tmux or closing the plugin but before removing the record,
        // the next reconciliation beat resumes this request instead of
        // leaving a half-cleaned mission behind.
        if mission.delete_requested.is_none() || mission.launch_requested.is_some() {
            mission.launch_requested = None;
            mission
                .delete_requested
                .get_or_insert(MissionDeleteRequest {
                    requested_at: self.clock.unix_seconds(),
                });
            self.store.update_mission(project, slug, &mission)?;
        }
        let needs_teardown =
            mission.session.is_some() || self.mission_environment_needs_cleanup(project, slug);
        if needs_teardown {
            match self.stop_mission(project, slug)? {
                StopMissionResult::Scheduled => {
                    self.pending_mission_deletes
                        .insert((project.to_string(), slug.to_string()));
                    Ok(DeleteMissionResult::Scheduled)
                }
                StopMissionResult::Completed(path) => {
                    drop(path);
                    self.store.delete_mission(project, slug)?;
                    Ok(DeleteMissionResult::Completed)
                }
            }
        } else {
            // A previous cleanup attempt may have failed in-process while an
            // external/plugin recovery subsequently closed the durable
            // environment. Once both durable handles are verified absent,
            // that failed phase is stale and must not require a UI-only
            // "Retry cleanup" command forever.
            let reconciled = self
                .run_phases
                .iter()
                .filter(|(id, _)| id.project == project && id.mission == slug)
                .max_by_key(|(id, _)| id.generation)
                .and_then(|(id, phase)| {
                    matches!(
                        phase,
                        RunPhase::Failed {
                            cleanup_pending: true,
                            ..
                        }
                    )
                    .then(|| id.clone())
                });
            if let Some(run_id) = reconciled {
                self.finish_run(&run_id);
            }
            if self.mission_run_inflight(project, slug) {
                return Err(Error::Store(
                    "mission launch or teardown is still in progress".into(),
                ));
            }
            self.store.delete_mission(project, slug)?;
            Ok(DeleteMissionResult::Completed)
        }
    }

    /// A mission's operator-facing label (from the cache): its name, else
    /// its human slug, else `new` — the same rule the nav uses. Never a raw
    /// uuid.
    pub fn mission_label(&self, slug: &str) -> String {
        let name = self
            .missions
            .iter()
            .find(|(s, _)| s == slug)
            .and_then(|(_, m)| m.name.clone());
        mission_label(name.as_deref(), slug)
    }

    /// An agent's operator-facing label from the selected project's cache:
    /// its name, else its human slug, else `unnamed agent` — never a raw
    /// uuid. Mirrors [`Self::mission_label`].
    pub fn agent_label(&self, slug: &str) -> String {
        let name = self
            .agents
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(_, a)| a.meta.name.clone())
            .unwrap_or_default();
        agent_label(&name, slug)
    }

    /// An agent label for durable run history. A deleted human-slug agent
    /// keeps that readable handle; an app-generated UUID is replaced with a
    /// lifecycle label instead of leaking storage identity into the UI.
    pub fn mission_log_agent_label(&self, slug: &str) -> String {
        let name = self
            .agents
            .iter()
            .find(|(candidate, _)| candidate == slug)
            .map(|(_, agent)| agent.meta.name.as_str());
        historical_agent_label(name, slug)
    }

    /// Keep the selected project's scoped caches — its agent list, its
    /// mission list, and the corpus summary — current on their own, so a
    /// change the CURATOR makes from the MCP process (deletes a mission,
    /// spawns an agent, writes a finding) just appears. Without this the
    /// lists refreshed only on the app's OWN CRUD or a reselect, so an
    /// external mutation stayed invisible until the operator clicked away
    /// and back. All three are cheap `read_dir` + `stat` passes (bounded
    /// by file COUNT, not size), so a throttle this tight is comfortable.
    /// Selection is held by slug and both views fall back when its target
    /// vanishes, so a background re-list never yanks the operator's cursor.
    /// Selection-change refreshes still happen immediately elsewhere; this
    /// only fills the gaps between them.
    pub fn poll_project_scope(&mut self) {
        let Some(project) = self.effective_project() else {
            return;
        };
        let now = self.clock.monotonic_now();
        let backstop = self.store_backstop();
        let due = self
            .corpus_polled_at
            .is_none_or(|t| now.saturating_duration_since(t) >= backstop);
        if due {
            if self.jobs.is_some() {
                self.corpus_polled_at = Some(now);
                self.prepare_findings_project(&project);
                let scope = self.corpus_job_scope(&project);
                let store = self.store.clone();
                let project_owned = project.clone();
                let mut finding_cache = self.finding_index_cache.clone();
                self.jobs.as_mut().expect("installed above").start(
                    JobKind::ProjectScope,
                    scope,
                    Duration::from_secs(30),
                    move |token| {
                        let agents = store
                            .list_agents(&project_owned)
                            .map_err(|error| error.to_string())?;
                        let missions = sort_missions(
                            store
                                .list_missions(&project_owned)
                                .map_err(|error| error.to_string())?,
                        );
                        let stats = corpus_core::corpus_stats(&store, &project_owned)
                            .map_err(|error| error.to_string())?;
                        let logs = corpus_core::mission_logs(&store, &project_owned)
                            .map_err(|error| error.to_string())?;
                        let findings = corpus_core::scan_findings_cached(
                            &store,
                            &project_owned,
                            &mut finding_cache,
                            || token.is_cancelled(),
                        )
                        .map_err(|error| error.to_string())?;
                        Ok(AppJobOutput::ProjectScope(ProjectScopeSnapshot {
                            agents,
                            missions,
                            stats,
                            logs,
                            findings: FindingSnapshot {
                                cards: findings.cards,
                                cache: finding_cache,
                            },
                        }))
                    },
                );
                return;
            }
            self.refresh_agents(&project);
            self.refresh_missions(&project);
            // Stamps `corpus_polled_at`, closing the throttle for all three.
            self.refresh_corpus_summary(&project);
        }
    }

    /// Launch a mission's run: a full opencode TUI in a detached tmux
    /// session, kicked off with the mission's BRIEF as the opencode
    /// `--prompt` (an empty brief lands at a bare prompt, the old
    /// behaviour — the sidebar `+` creates briefless missions on purpose).
    /// The spawned tmux session is persisted on the mission record so a
    /// relaunched app re-attaches by selection. A live run is BACKGROUNDED,
    /// not replaced: it keeps running under its own tmux session (and its
    /// own mission record), while this mission lands on a fresh opencode
    /// session that the operator watches and steers in the embedded pane.
    ///
    /// This ADOPTS the run as the app-owned one, so the mission view
    /// attaches its pane immediately — the path for an operator who clicked
    /// Launch and wants to watch. A curator's autonomous launch takes
    /// `launch_mission_detached` instead, which does not hijack the pane.
    /// The model the launch dialog pre-fills: the registry's curated
    /// tool-use default (an explicit arg — the engine never falls back
    /// to opencode's ambient model). None when the registry is empty.
    pub fn suggested_model(&self) -> Option<String> {
        corpus_core::ModelRegistry::load_default()
            .ok()?
            .launch_default()
    }

    /// The launch pre-fill for an agent: primary entry model → registry
    /// tool-use default.
    pub fn agent_default_model(&self, project: &str, agent: &str) -> Option<String> {
        corpus_core::launch::agent_default_model(&self.store, project, agent)
            .or_else(|| self.suggested_model())
    }
}

/// The label to show for an agent: its display name, never an opaque
/// UUID slug.
///
/// A real name always wins. "unnamed agent" is only for an agent with no
/// meaningful handle: an empty name, or a name equal to the app's own
/// UUID slug — which the `+` flow writes into the sidecar before it stamps
/// the placeholder, so a raw id never surfaces if that stamp is lost.
///
/// The `name == slug` case is qualified by UUID-shape ON PURPOSE. The
/// curator names an agent by a human slug (`reporter`), and `create_agent`
/// records that slug as the name — so `name == slug` there is a REAL name,
/// not a missing one. Collapsing it unconditionally is what made every
/// curator-built agent read as "unnamed agent" while its own form showed
/// the name. The slug stays in hover tooltips and the JSON tab for identity.
pub fn agent_label(name: &str, slug: &str) -> String {
    if name.is_empty() || (name == slug && is_uuid_like(slug)) {
        "unnamed agent".to_string()
    } else {
        name.to_string()
    }
}

pub(super) fn historical_agent_label(name: Option<&str>, slug: &str) -> String {
    match name {
        Some(name) => agent_label(name, slug),
        None if is_uuid_like(slug) => "deleted agent".to_string(),
        None => slug.to_string(),
    }
}

/// The label to show for a mission in the nav: its display name, else its
/// slug when that is a human handle (the curator names a mission
/// `cdk-proto-attack` — show it), else `new` for the app's own UUID-slug
/// missions created before they are named. Mirrors [`agent_label`].
pub fn mission_label(name: Option<&str>, slug: &str) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None if !is_uuid_like(slug) => slug.to_string(),
        None => "new".to_string(),
    }
}

/// Whether a slug is one of the app's generated UUIDs (see `new_uuid_id`):
/// 36 chars, `8-4-4-4-12` hex with dashes at the canonical offsets. A human
/// slug (`reporter`, `recon-mapper`) never matches, so it is never mistaken
/// for a placeholder id.
pub(super) fn is_uuid_like(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// Mission list order, newest-CREATED first (slug tiebreak). The store
/// returns slug order — stable across saves, but the slugs are random
/// UUIDs so the sidebar looked shuffled; created order matches the
/// project list (state.rs `refresh`, newest first).
pub(super) fn sort_missions(mut missions: Vec<(String, Mission)>) -> Vec<(String, Mission)> {
    missions.sort_by(|a, b| b.1.created.cmp(&a.1.created).then_with(|| a.0.cmp(&b.0)));
    missions
}
