//! Curator dispatch requests, completion detection, and durable delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use corpus_core::{Error, Store};

use super::{
    launch_stamp_ms, mission_label, mission_session_ref, mission_workspace_candidates,
    orphan_environment_sessions, AppJobOutput, AppState, DeleteMissionResult, LaunchNotice,
    PromptDeliveryState, SessionService, SessionTurnState,
};
use crate::jobs::{JobKind, JobScope};
use crate::observability::{DeliveryEvent, DeliveryTerminal};

pub(super) struct LaunchRequest {
    pub(super) project: String,
    pub(super) slug: String,
    pub(super) label: String,
    pub(super) already_live: bool,
}

pub(super) struct DeletionRequest {
    pub(super) project: String,
    pub(super) slug: String,
}

pub(super) struct AgentDeletionRequest {
    pub(super) project: String,
    pub(super) agent: String,
}

impl AppState {
    /// Refresh session identity/liveness and run the production completion
    /// and curator-delivery reconcilers once. This does not honor new launch
    /// requests, allowing a single-model runner to wait for a terminal turn
    /// before starting the next process.
    pub fn reconcile_headless_sessions(&mut self) -> Result<(), String> {
        debug_assert!(self.jobs.is_none());
        self.refresh_live_sessions();
        let projects = self
            .store
            .list_projects()
            .map_err(|error| error.to_string())?;
        for (project, _) in projects {
            self.bind_headless_conversations(&project)?;
        }
        reconcile_dispatch_activity(
            &self.store,
            self.session_service.as_ref(),
            &self.live_sessions,
        )?;
        deliver_completed_dispatches(
            &self.store,
            self.session_service.as_ref(),
            &self.live_sessions,
        )
    }

    fn bind_headless_conversations(&mut self, project: &str) -> Result<(), String> {
        let missions = self
            .store
            .list_missions(project)
            .map_err(|error| error.to_string())?;
        let mut claimed = missions
            .iter()
            .filter_map(|(_, mission)| mission.opencode_session.clone())
            .collect::<BTreeSet<_>>();
        for (slug, mut mission) in missions {
            if mission.opencode_session.is_some() && mission.opencode_workspace.is_some() {
                continue;
            }
            let Some(run_id) = mission.session.as_deref() else {
                continue;
            };
            if !self.live_sessions.iter().any(|live| live == run_id) {
                continue;
            }
            let launched_at_ms = launch_stamp_ms(run_id)
                .ok_or_else(|| format!("invalid Corpus run identity: {run_id}"))?;
            let workspaces = mission_workspace_candidates(&self.store, project, &mission)?;
            let identity = match mission.opencode_session.clone() {
                Some(conversation) => self
                    .session_service
                    .find_session_workspace(&workspaces, &conversation)
                    .map(|workspace| (conversation, workspace)),
                None => self.session_service.find_for_launch_in_workspaces(
                    &workspaces,
                    launched_at_ms,
                    &claimed,
                ),
            };
            let (conversation, workspace) = match identity {
                Ok(identity) => identity,
                Err(error) if error == "no OpenCode session found for this launch" => continue,
                Err(error) => return Err(error),
            };
            claimed.insert(conversation.clone());
            mission.opencode_session = Some(conversation);
            mission.opencode_workspace = Some(workspace);
            self.store
                .update_mission(project, &slug, &mission)
                .map_err(|error| error.to_string())?;
        }
        self.refresh_missions(project);
        Ok(())
    }

    /// Honor currently durable curator launch requests immediately. Callers
    /// must first prove the requesting turn is terminal on single-model hosts.
    pub fn honor_headless_launch_requests(&mut self) {
        debug_assert!(self.jobs.is_none());
        self.launch_requests_polled_at = None;
        self.poll_launch_requests();
    }

    /// Whether the exact mission's latest launch turn has reached a terminal
    /// non-tool assistant step in its owning OpenCode process.
    pub fn mission_turn_completed(&self, project: &str, slug: &str) -> Result<bool, String> {
        let mission = self
            .store
            .load_mission(project, slug)
            .map_err(|error| error.to_string())?;
        let run_id = mission
            .session
            .as_deref()
            .ok_or_else(|| format!("{project}/{slug} has no run identity"))?;
        let control = mission
            .control
            .as_ref()
            .filter(|control| control.run_id == run_id)
            .ok_or_else(|| format!("{project}/{slug} has no matching control endpoint"))?;
        mission
            .opencode_session
            .as_ref()
            .ok_or_else(|| format!("{project}/{slug} has no OpenCode session yet"))?;
        let password = corpus_core::opencode_control_password(&self.store, run_id)
            .map_err(|error| error.to_string())?;
        let session = mission_session_ref(&self.store, project, &mission)?;
        let launched_at_ms = launch_stamp_ms(run_id).unwrap_or(0);
        self.session_service
            .session_turn_state(control, &password, &session, launched_at_ms)
            .map(|state| state == SessionTurnState::Completed)
    }

    /// Deliver terminal child results through each exact parent TUI. The
    /// worker owns all HTTP and store I/O; the render loop only schedules a
    /// coalesced global pass after liveness/conversation reconciliation.
    pub(super) fn schedule_dispatch_deliveries(&mut self) {
        let Some(jobs) = self.jobs.as_mut() else {
            return;
        };
        let store = self.store.clone();
        let service = self.session_service.clone();
        let live = self.live_sessions.clone();
        jobs.start(
            JobKind::DispatchDelivery,
            JobScope {
                project: String::new(),
                project_generation: 0,
                corpus_revision: None,
                run_id: None,
            },
            Duration::from_secs(10),
            move |_| {
                reconcile_dispatch_activity(&store, service.as_ref(), &live)?;
                deliver_completed_dispatches(&store, service.as_ref(), &live)?;
                Ok(AppJobOutput::DispatchDeliveries)
            },
        );
    }

    /// Honor any launch the CURATOR requested (its `mission_launch` tool
    /// set `launch_requested` on a mission record from the MCP process —
    /// run spawning is the app's alone). Filesystem notifications make the
    /// scan due promptly; the slower timer reconciles missed events. The scan
    /// reads every project's mission records because the flag was written by
    /// another process and the cached tree does not have it.
    ///
    /// The request is cleared BEFORE the spawn, so a launch that fails
    /// reports once instead of retrying every beat. A mission whose
    /// requested session is already live just clears — the curator asked
    /// for a run and there is one. Scans EVERY project, not just the
    /// selected one: the curator is scoped to its own project, which the
    /// operator need not be viewing when the launch fires.
    pub fn poll_launch_requests(&mut self) {
        let now = self.clock.monotonic_now();
        let backstop = self.store_backstop();
        let due = self
            .launch_requests_polled_at
            .is_none_or(|t| now.saturating_duration_since(t) >= backstop);
        if !due {
            return;
        }
        self.launch_requests_polled_at = Some(now);
        self.reconcile_orphan_environments();

        // Gather flagged missions off disk first (the authoritative record —
        // the flag came from the MCP process).
        let projects = self
            .store
            .list_projects()
            .unwrap_or_else(|_| self.projects.clone());
        if let Some(jobs) = self.jobs.as_mut() {
            let store = self.store.clone();
            let catalog = self.session_catalog.clone();
            jobs.start(
                JobKind::LaunchRequests,
                JobScope {
                    project: String::new(),
                    project_generation: 0,
                    corpus_revision: None,
                    run_id: None,
                },
                Duration::from_secs(15),
                move |_| {
                    let live = catalog.live_tui_sessions();
                    let mut launches = Vec::new();
                    let mut deletions = Vec::new();
                    let mut agent_deletions = Vec::new();
                    let mut project_deletions = Vec::new();
                    for (project, project_record) in projects {
                        let project_deleting = project_record.delete_requested.is_some();
                        if project_deleting {
                            project_deletions.push(project.clone());
                        }
                        let deleting_agents: BTreeSet<String> = store
                            .list_agents(&project)
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|(agent, config)| {
                                config.meta.delete_requested.is_some().then_some(agent)
                            })
                            .collect();
                        agent_deletions.extend(deleting_agents.iter().map(|agent| {
                            AgentDeletionRequest {
                                project: project.clone(),
                                agent: agent.clone(),
                            }
                        }));
                        let Ok(missions) = store.list_missions(&project) else {
                            continue;
                        };
                        for (slug, mission) in missions {
                            if mission.delete_requested.is_some() {
                                deletions.push(DeletionRequest {
                                    project: project.clone(),
                                    slug,
                                });
                                continue;
                            }
                            if project_deleting || deleting_agents.contains(&mission.agent) {
                                continue;
                            }
                            if mission.launch_requested.is_none() {
                                continue;
                            }
                            let already_live = mission
                                .session
                                .as_deref()
                                .is_some_and(|session| live.iter().any(|item| item == session));
                            launches.push(LaunchRequest {
                                project: project.clone(),
                                label: mission_label(mission.name.as_deref(), &slug),
                                slug,
                                already_live,
                            });
                        }
                    }
                    Ok(AppJobOutput::LaunchRequests {
                        launches,
                        deletions,
                        agent_deletions,
                        project_deletions,
                    })
                },
            );
            return;
        }
        let mut pending: Vec<(String, String, Option<String>)> = Vec::new();
        let mut deletions = Vec::new();
        let mut agent_deletions = Vec::new();
        let mut project_deletions = Vec::new();
        for (project, project_record) in &projects {
            let project_deleting = project_record.delete_requested.is_some();
            if project_deleting {
                project_deletions.push(project.clone());
            }
            let deleting_agents: BTreeSet<String> = self
                .store
                .list_agents(project)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(agent, config)| {
                    config.meta.delete_requested.is_some().then_some(agent)
                })
                .collect();
            agent_deletions.extend(deleting_agents.iter().map(|agent| AgentDeletionRequest {
                project: project.clone(),
                agent: agent.clone(),
            }));
            let Ok(missions) = self.store.list_missions(project) else {
                continue;
            };
            for (slug, m) in missions {
                if m.delete_requested.is_some() {
                    deletions.push(DeletionRequest {
                        project: project.clone(),
                        slug,
                    });
                    continue;
                }
                if project_deleting || deleting_agents.contains(&m.agent) {
                    continue;
                }
                if m.launch_requested.is_some() {
                    pending.push((project.clone(), slug, m.session.clone()));
                }
            }
        }
        if pending.is_empty()
            && deletions.is_empty()
            && agent_deletions.is_empty()
            && project_deletions.is_empty()
        {
            return;
        }
        self.apply_deletion_requests(deletions);
        self.apply_parent_deletion_requests(agent_deletions, project_deletions);
        // A fresh listing so "already live" is a real answer, not a stale
        // one that would spawn a duplicate.
        self.refresh_live_sessions();
        for (project, slug, session) in pending {
            let already_live = session
                .as_deref()
                .is_some_and(|s| self.live_sessions.iter().any(|l| l == s));
            // Clear FIRST: a spawn failure must not loop the request.
            if let Err(error) = self.clear_launch_request(&project, &slug, !already_live) {
                self.launch_notices.push(LaunchNotice {
                    mission: slug.clone(),
                    result: Err(error.to_string()),
                });
                continue;
            }
            if already_live {
                continue;
            }
            let label = self.mission_display_label(&project, &slug);
            let result = self
                .launch_mission_detached(&project, &slug)
                .map_err(|error| {
                    let message = error.to_string();
                    self.record_dispatch_launch_failure(&project, &slug, &message);
                    message
                });
            self.launch_notices.push(LaunchNotice {
                mission: label,
                result,
            });
        }
    }

    pub(super) fn apply_mission_requests(
        &mut self,
        deletions: Vec<DeletionRequest>,
        agent_deletions: Vec<AgentDeletionRequest>,
        project_deletions: Vec<String>,
        launches: Vec<LaunchRequest>,
    ) {
        self.apply_deletion_requests(deletions);
        self.apply_parent_deletion_requests(agent_deletions, project_deletions);
        for request in launches {
            if let Err(error) =
                self.clear_launch_request(&request.project, &request.slug, !request.already_live)
            {
                self.launch_notices.push(LaunchNotice {
                    mission: request.label,
                    result: Err(error.to_string()),
                });
                continue;
            }
            if request.already_live {
                continue;
            }
            if let Err(error) = self.launch_mission_detached(&request.project, &request.slug) {
                self.record_dispatch_launch_failure(
                    &request.project,
                    &request.slug,
                    &error.to_string(),
                );
                self.launch_notices.push(LaunchNotice {
                    mission: request.label,
                    result: Err(error.to_string()),
                });
            }
        }
    }

    fn apply_deletion_requests(&mut self, requests: Vec<DeletionRequest>) {
        let mut refresh = BTreeSet::new();
        for request in requests {
            match self.delete_mission(&request.project, &request.slug) {
                Ok(DeleteMissionResult::Completed) => {
                    refresh.insert(request.project);
                }
                Ok(DeleteMissionResult::Scheduled) | Err(_) => {}
            }
        }
        for project in refresh {
            self.refresh_missions(&project);
        }
    }

    fn reconcile_orphan_environments(&mut self) {
        for (plugin, key) in orphan_environment_sessions(&self.store) {
            let _ = self.cleanup_orphan_environment(&plugin, &key);
        }
    }

    fn apply_parent_deletion_requests(
        &mut self,
        agent_requests: Vec<AgentDeletionRequest>,
        project_requests: Vec<String>,
    ) {
        let project_set: BTreeSet<String> = project_requests.iter().cloned().collect();
        for request in agent_requests {
            if project_set.contains(&request.project) {
                continue;
            }
            let missions = self
                .store
                .missions_for_agent(&request.project, &request.agent)
                .unwrap_or_default();
            for mission in missions {
                let _ = self.delete_mission(&request.project, &mission);
            }
            if self
                .store
                .missions_for_agent(&request.project, &request.agent)
                .is_ok_and(|missions| missions.is_empty())
            {
                let _ = self.store.delete_agent(&request.project, &request.agent);
                self.refresh_agents(&request.project);
            }
        }
        for project in project_requests {
            let missions = self.store.list_missions(&project).unwrap_or_default();
            for (mission, _) in missions {
                let _ = self.delete_mission(&project, &mission);
            }
            if self
                .store
                .list_missions(&project)
                .is_ok_and(|missions| missions.is_empty())
            {
                match self.store.delete_project(&project) {
                    Ok(()) => {
                        self.prune_project_cache(&project);
                        self.refresh();
                    }
                    Err(error) => {
                        self.pending_background_notices
                            .push(crate::state::BackgroundNotice::error(
                                JobKind::LaunchRequests,
                                format!("could not finish deleting project {project}: {error}"),
                            ))
                    }
                }
            }
        }
    }

    /// Drain the curator-launch reports queued since the last call — the
    /// app loop turns each into a toast.
    pub fn take_launch_notices(&mut self) -> Vec<LaunchNotice> {
        std::mem::take(&mut self.launch_notices)
    }

    /// Consume a mission's launch request before spawning. The proven parent
    /// origin is copied onto the child record in the same write, so a spawn
    /// failure or app restart cannot lose the return path.
    pub(super) fn clear_launch_request(
        &mut self,
        project: &str,
        slug: &str,
        bind_origin: bool,
    ) -> Result<(), Error> {
        self.store
            .consume_mission_launch_request(project, slug, bind_origin)
            .map(drop)
    }

    pub(super) fn record_dispatch_launch_failure(
        &mut self,
        project: &str,
        slug: &str,
        error: &str,
    ) {
        let bounded: String = error.chars().take(2_000).collect();
        let _ = self.record_dispatch_completion(
            project,
            slug,
            corpus_core::MissionCompletion::LaunchFailed {
                at: self.clock.unix_seconds(),
                error: bounded,
            },
        );
    }

    /// Persist one terminal child result. Once present it is immutable, so
    /// repeated liveness scans and app restarts cannot create duplicates.
    pub(super) fn record_dispatch_completion(
        &mut self,
        project: &str,
        slug: &str,
        completion: corpus_core::MissionCompletion,
    ) -> Result<bool, Error> {
        self.store
            .record_mission_dispatch_completion(project, slug, completion)
    }

    /// Fold the existing tmux/raw-capture state into durable dispatch facts.
    /// This starts no inference and performs no delivery. An initial parked
    /// session is only remembered as live; completion requires that the exact
    /// child run was first observed producing output.
    pub(super) fn reconcile_mission_dispatches(&mut self) {
        let Ok(projects) = self.store.list_projects() else {
            return;
        };
        let now = self.clock.unix_seconds();
        for (project, _) in projects {
            let Ok(missions) = self.store.list_missions(&project) else {
                continue;
            };
            for (slug, mut mission) in missions {
                let Some(snapshot) = mission.dispatch.as_ref() else {
                    continue;
                };
                if snapshot.completion.is_some() {
                    continue;
                }
                let Some(child_run_id) = snapshot.child_run_id.as_deref() else {
                    continue;
                };
                // Piped children have no tmux binding; `poll_run` owns their
                // exact process exit and records completion there.
                if mission.session.is_none() {
                    continue;
                }
                // Only the exact bound child may advance this dispatch. A
                // later manual relaunch cannot complete an older request.
                let exact_binding = mission.session.as_deref() == Some(child_run_id);
                let live = exact_binding
                    && self
                        .live_sessions
                        .iter()
                        .any(|session| session == child_run_id);
                let dispatch = mission.dispatch.as_mut().expect("checked above");
                let mut changed = false;
                if live && !dispatch.live_seen {
                    dispatch.live_seen = true;
                    changed = true;
                }
                // Terminal-paint recency is only a display signal. A quiet
                // provider or tool may produce no PTY bytes for many seconds,
                // so successful completion is recorded off-thread from the
                // exact OpenCode process's active-session endpoint instead.
                if !live && dispatch.live_seen {
                    dispatch.completion =
                        Some(corpus_core::MissionCompletion::UnexpectedExit { at: now });
                    changed = true;
                }
                if changed {
                    let _ = self.store.update_mission(&project, &slug, &mission);
                }
            }
        }
    }

    /// A mission's operator-facing label from its DISK record (name, else
    /// its human slug, else `new`) — the cache covers only the selected
    /// project, and a curator launch can name any.
    pub(super) fn mission_display_label(&self, project: &str, slug: &str) -> String {
        let name = self
            .store
            .load_mission(project, slug)
            .ok()
            .and_then(|m| m.name);
        mission_label(name.as_deref(), slug)
    }
}

#[derive(Clone)]
struct DispatchDeliveryItem {
    project: String,
    slug: String,
    child_run_id: Option<String>,
    completion: corpus_core::MissionCompletion,
    delivery_attempt: u32,
    delivery_message_id: Option<String>,
}

impl DispatchDeliveryItem {
    fn identity(
        &self,
        parent: &corpus_core::MissionRunRef,
    ) -> corpus_core::MissionDispatchIdentity {
        corpus_core::MissionDispatchIdentity {
            parent: parent.clone(),
            child_run_id: self.child_run_id.clone(),
            completion: self.completion.clone(),
        }
    }
}

/// Advance dispatched children from running to completed using the private
/// endpoint owned by each exact child TUI. A live-but-quiet terminal is not a
/// terminal event: intermediate assistant messages ending in `tool-calls`
/// remain active, while the first later non-tool finish is the restart-safe
/// whole-loop completion proof.
pub(super) fn reconcile_dispatch_activity(
    store: &Store,
    service: &dyn SessionService,
    live: &[String],
) -> Result<(), String> {
    let projects = store.list_projects().map_err(|error| error.to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut failures = Vec::new();
    for (project, _) in projects {
        let missions = match store.list_missions(&project) {
            Ok(missions) => missions,
            Err(error) => {
                failures.push(format!("{project}: {error}"));
                continue;
            }
        };
        for (slug, mut mission) in missions {
            let Some(snapshot) = mission.dispatch.as_ref() else {
                continue;
            };
            if snapshot.completion.is_some() {
                continue;
            }
            let Some(child_run_id) = snapshot.child_run_id.as_deref() else {
                continue;
            };
            let exact_live = mission.session.as_deref() == Some(child_run_id)
                && live.iter().any(|session| session == child_run_id);
            if !exact_live {
                continue;
            }
            let Some(control) = mission
                .control
                .as_ref()
                .filter(|control| control.run_id == child_run_id)
            else {
                continue;
            };
            let Some(_conversation) = mission.opencode_session.as_ref() else {
                continue;
            };
            let password = match corpus_core::opencode_control_password(store, child_run_id) {
                Ok(password) => password,
                Err(error) => {
                    failures.push(format!("{project}/{slug}: {error}"));
                    continue;
                }
            };
            let session = match mission_session_ref(store, &project, &mission) {
                Ok(session) => session,
                Err(error) => {
                    failures.push(format!("{project}/{slug}: {error}"));
                    continue;
                }
            };
            let launched_at_ms = launch_stamp_ms(child_run_id).unwrap_or(0);
            let turn_state =
                match service.session_turn_state(control, &password, &session, launched_at_ms) {
                    Ok(state) => state,
                    Err(error) => {
                        failures.push(format!("{project}/{slug}: {error}"));
                        continue;
                    }
                };
            let dispatch = mission.dispatch.as_mut().expect("checked above");
            let changed = match turn_state {
                SessionTurnState::Active if !dispatch.running_seen => {
                    dispatch.running_seen = true;
                    true
                }
                SessionTurnState::Completed => {
                    dispatch.running_seen = true;
                    dispatch.completion = Some(corpus_core::MissionCompletion::Completed {
                        at: now,
                        artifacts: Vec::new(),
                    });
                    true
                }
                SessionTurnState::Pending | SessionTurnState::Active => false,
            };
            if changed {
                if let Err(error) = store.update_mission(&project, &slug, &mission) {
                    failures.push(format!("{project}/{slug}: {error}"));
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "mission completion delivery failed: {}",
            failures.join("; ")
        ))
    }
}

/// Reconcile persisted completion deliveries, then group newly completed
/// children by launcher-proven parent and resume each exact curator. Intent,
/// admission, and acknowledgement are deliberately separate: a stored id can
/// wait safely while the curator is active, and only a terminal assistant
/// response marks delivery complete.
pub(super) fn deliver_completed_dispatches(
    store: &Store,
    service: &dyn SessionService,
    live: &[String],
) -> Result<(), String> {
    let projects = store.list_projects().map_err(|error| error.to_string())?;
    let mut groups: BTreeMap<corpus_core::MissionRunRef, Vec<DispatchDeliveryItem>> =
        BTreeMap::new();
    for (project, _) in projects {
        let Ok(missions) = store.list_missions(&project) else {
            continue;
        };
        for (slug, mission) in missions {
            let Some(dispatch) = mission.dispatch else {
                continue;
            };
            let Some(completion) = dispatch.completion else {
                continue;
            };
            if dispatch.delivery_abandoned.is_some() {
                continue;
            }
            // Records written by the old admission-is-delivery implementation
            // have `delivered=true` but no message id. They were never
            // acknowledged and must be repaired rather than silently skipped.
            if dispatch.delivered && dispatch.delivery_message_id.is_some() {
                continue;
            }
            groups
                .entry(dispatch.parent)
                .or_default()
                .push(DispatchDeliveryItem {
                    project: project.clone(),
                    slug,
                    child_run_id: dispatch.child_run_id,
                    completion,
                    delivery_attempt: dispatch.delivery_attempt,
                    delivery_message_id: dispatch.delivery_message_id,
                });
        }
    }
    if groups.is_empty() {
        return Ok(());
    }
    let mut failures = Vec::new();
    for (parent, mut children) in groups {
        children
            .sort_by(|left, right| (&left.project, &left.slug).cmp(&(&right.project, &right.slug)));
        let Ok(parent_mission) = store.load_mission(&parent.project, &parent.mission) else {
            continue;
        };
        let Some(control) = parent_mission.control.as_ref() else {
            continue;
        };
        if parent_mission.session.as_deref() != Some(parent.run_id.as_str())
            || control.run_id != parent.run_id
            || !live.iter().any(|session| session == &parent.run_id)
        {
            continue;
        }
        let Some(_conversation) = parent_mission.opencode_session.as_ref() else {
            continue;
        };
        let Ok(password) = corpus_core::opencode_control_password(store, &control.run_id) else {
            continue;
        };

        let session = match mission_session_ref(store, &parent.project, &parent_mission) {
            Ok(session) => session,
            Err(error) => {
                failures.push(format!("{}/{}: {error}", parent.project, parent.mission));
                continue;
            }
        };

        let mut admitted: BTreeMap<String, Vec<DispatchDeliveryItem>> = BTreeMap::new();
        let mut pending = Vec::new();
        for child in children {
            if let Some(message_id) = child.delivery_message_id.clone() {
                admitted.entry(message_id).or_default().push(child);
            } else {
                pending.push(child);
            }
        }
        for (message_id, attempted) in admitted {
            let started = Instant::now();
            let attempt = attempted
                .iter()
                .map(|child| child.delivery_attempt)
                .max()
                .unwrap_or(0);
            match service.prompt_delivery_state(control, &password, &session, &message_id) {
                Ok(PromptDeliveryState::Acknowledged) => {
                    let mut persisted = true;
                    for child in &attempted {
                        persisted = mark_dispatch_acknowledged(store, &parent, child, &message_id)
                            && persisted;
                    }
                    let event = DeliveryEvent::new(
                        &parent,
                        &message_id,
                        attempt,
                        attempted.len(),
                        started.elapsed(),
                    );
                    if persisted {
                        event.emit(DeliveryTerminal::Acknowledged, false, "");
                    } else {
                        let error = "curator acknowledged completion prompt but its durable delivery state could not be persisted";
                        event.emit(DeliveryTerminal::PersistenceFailed, true, error);
                        failures.push(format!("{}/{}: {error}", parent.project, parent.mission));
                    }
                }
                Ok(PromptDeliveryState::Failed {
                    error,
                    retry_ready,
                    interrupted,
                }) => {
                    if interrupted {
                        let mut persisted = true;
                        for child in &attempted {
                            persisted = mark_dispatch_abandoned(store, &parent, child, &message_id)
                                && persisted;
                        }
                        let terminal = if persisted {
                            DeliveryTerminal::Abandoned
                        } else {
                            DeliveryTerminal::PersistenceFailed
                        };
                        DeliveryEvent::new(
                            &parent,
                            &message_id,
                            attempt,
                            attempted.len(),
                            started.elapsed(),
                        )
                        .emit(terminal, !persisted, &error);
                        if !persisted {
                            failures.push(format!(
                                "{}/{}: interrupted completion delivery could not be durably abandoned",
                                parent.project, parent.mission
                            ));
                        }
                        continue;
                    }
                    // Keep the failed admission durable. Re-posting the same
                    // id cannot restart it, while immediately minting ids in a
                    // loop can burn credits. A deliberate model switch is the
                    // event that permits attempt N+1.
                    let mut persisted = true;
                    if retry_ready {
                        for child in &attempted {
                            persisted = mark_dispatch_retryable(store, &parent, child, &message_id)
                                && persisted;
                        }
                    }
                    let terminal = if !persisted {
                        DeliveryTerminal::PersistenceFailed
                    } else if retry_ready {
                        DeliveryTerminal::RetryReady
                    } else {
                        DeliveryTerminal::Failed
                    };
                    DeliveryEvent::new(
                        &parent,
                        &message_id,
                        attempt,
                        attempted.len(),
                        started.elapsed(),
                    )
                    .emit(terminal, retry_ready, &error);
                    failures.push(format!(
                        "{}/{}: curator did not handle completion prompt: {error}{}",
                        parent.project,
                        parent.mission,
                        if retry_ready {
                            "; retrying after model switch"
                        } else {
                            ""
                        }
                    ));
                }
                Ok(PromptDeliveryState::Pending) => {
                    // A persisted id with no legacy user message is either a
                    // delivery deferred while the curator was active or an
                    // admission made by the unusable V2 queue path. Re-submit
                    // the same deterministic id; the adapter preflights the
                    // transcript, so this is idempotent.
                    let prompt = completion_prompt(&attempted);
                    if let Err(error) =
                        service.queue_prompt(control, &password, &session, &message_id, &prompt)
                    {
                        failures.push(format!("{}/{}: {error}", parent.project, parent.mission));
                    }
                }
                Ok(PromptDeliveryState::Active) => {}
                Err(error) => {
                    DeliveryEvent::new(
                        &parent,
                        &message_id,
                        attempt,
                        attempted.len(),
                        started.elapsed(),
                    )
                    .emit(DeliveryTerminal::StatusError, true, &error);
                    failures.push(format!("{}/{}: {error}", parent.project, parent.mission))
                }
            }
        }
        if pending.is_empty() {
            continue;
        }

        let attempt = pending
            .iter()
            .map(|child| child.delivery_attempt)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let signature = pending
            .iter()
            .map(|child| {
                format!(
                    "{}/{}:{}:{}",
                    child.project,
                    child.slug,
                    child.child_run_id.as_deref().unwrap_or("launch_failed"),
                    serde_json::to_string(&child.completion).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        let message_id = format!(
            "msg_corpus{}",
            corpus_core::fnv1a_hex(
                format!(
                    "{}/{}/{}|attempt={attempt}|{signature}",
                    parent.project, parent.mission, parent.run_id,
                )
                .as_bytes()
            )
        );
        let prompt = completion_prompt(&pending);
        match service.queue_prompt(control, &password, &session, &message_id, &prompt) {
            Ok(()) => {
                for child in &pending {
                    if !mark_dispatch_admitted(store, &parent, child, attempt, &message_id) {
                        failures.push(format!(
                            "{}/{}: admitted completion prompt but could not persist its state",
                            child.project, child.slug
                        ));
                    }
                }
            }
            Err(error) => {
                failures.push(format!("{}/{}: {error}", parent.project, parent.mission));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "mission completion delivery failed: {}",
            failures.join("; ")
        ))
    }
}

fn mark_dispatch_admitted(
    store: &Store,
    parent: &corpus_core::MissionRunRef,
    child: &DispatchDeliveryItem,
    attempt: u32,
    message_id: &str,
) -> bool {
    store
        .admit_mission_dispatch_delivery(
            &child.project,
            &child.slug,
            &child.identity(parent),
            attempt,
            message_id,
        )
        .unwrap_or(false)
}

fn mark_dispatch_abandoned(
    store: &Store,
    parent: &corpus_core::MissionRunRef,
    child: &DispatchDeliveryItem,
    message_id: &str,
) -> bool {
    store
        .abandon_mission_dispatch_delivery(
            &child.project,
            &child.slug,
            &child.identity(parent),
            message_id,
        )
        .unwrap_or(false)
}

fn mark_dispatch_acknowledged(
    store: &Store,
    parent: &corpus_core::MissionRunRef,
    child: &DispatchDeliveryItem,
    message_id: &str,
) -> bool {
    store
        .acknowledge_mission_dispatch_delivery(
            &child.project,
            &child.slug,
            &child.identity(parent),
            message_id,
        )
        .unwrap_or(false)
}

fn mark_dispatch_retryable(
    store: &Store,
    parent: &corpus_core::MissionRunRef,
    child: &DispatchDeliveryItem,
    message_id: &str,
) -> bool {
    store
        .retry_mission_dispatch_delivery(
            &child.project,
            &child.slug,
            &child.identity(parent),
            message_id,
        )
        .unwrap_or(false)
}

fn completion_summary(completion: &corpus_core::MissionCompletion) -> String {
    match completion {
        corpus_core::MissionCompletion::Completed { artifacts, .. } if artifacts.is_empty() => {
            "completed".into()
        }
        corpus_core::MissionCompletion::Completed { artifacts, .. } => {
            format!("completed; new artifacts: {}", artifacts.join(", "))
        }
        corpus_core::MissionCompletion::UnexpectedExit { .. } => "exited unexpectedly".into(),
        corpus_core::MissionCompletion::LaunchFailed { error, .. } => {
            let bounded = error.chars().take(240).collect::<String>();
            format!("launch failed: {bounded}")
        }
    }
}

fn completion_prompt(children: &[DispatchDeliveryItem]) -> String {
    let mut prompt = String::from(
        "[Corpus mission completions]\nThe missions you dispatched have finished. Continue your current work using these results; do not poll mission status.\n",
    );
    for child in children {
        prompt.push_str(&format!(
            "- {}/{}: {}\n",
            child.project,
            child.slug,
            completion_summary(&child.completion)
        ));
    }
    prompt
}
