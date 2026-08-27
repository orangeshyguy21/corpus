//! External session discovery, maintenance, activity, and repaint projection.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use corpus_core::Store;

use super::{
    launch_stamp_ms, load_launchable_mission, mission_session_ref, mission_workspace_candidates,
    mission_workspace_dir, AppJobOutput, AppState, MissionActivity, MissionDisplayState,
    MissionStatusObservation, OpenCodeSessionStatus, RunPhase, SessionRef, SessionStatusUpdate,
    ACTIVITY_BACKSTOP, ACTIVITY_EVENT_MIN, LIVE_SESSION_BACKSTOP, OWNED_RUN_REPAINT_BACKSTOP,
    SESSION_RECONCILE_BACKSTOP, SESSION_STATUS_BUSY_POLL, SESSION_STATUS_GRACE,
    SESSION_STATUS_IDLE_POLL,
};
use crate::jobs::{JobKind, JobScope};

pub(super) fn mission_display_state_from(
    activity: MissionActivity,
    phase: &RunPhase,
    queued: bool,
    deleting: bool,
) -> MissionDisplayState {
    if deleting {
        return MissionDisplayState::Deleting;
    }
    match phase {
        RunPhase::Failed { .. } => MissionDisplayState::Failed,
        RunPhase::Preparing => MissionDisplayState::Preparing,
        RunPhase::Starting => MissionDisplayState::Starting,
        RunPhase::Stopping => MissionDisplayState::Stopping,
        RunPhase::Exporting => MissionDisplayState::Exporting,
        RunPhase::Idle | RunPhase::Running => match activity {
            MissionActivity::Idle if queued => MissionDisplayState::Queued,
            MissionActivity::Idle => MissionDisplayState::Idle,
            MissionActivity::Waiting => MissionDisplayState::Waiting,
            MissionActivity::Working => MissionDisplayState::Working,
        },
    }
}

/// Legacy/headless activity from the app's aged capture reading: turns a
/// `last_paint` Instant into idle-seconds and defers to the shared core rule.
/// Controlled UI missions use the owning OpenCode status endpoint instead.
pub(super) fn activity_for(
    now: Instant,
    live: bool,
    last_paint: Option<Instant>,
) -> MissionActivity {
    corpus_core::activity_from_idle(
        live,
        last_paint.map(|paint| now.saturating_duration_since(paint).as_secs()),
    )
}

/// The turn-completion export gate: given a `Waiting` session's last paint
/// and the moment we last exported it, should we re-export now? Yes only when
/// it painted output more recently than our last export (a real turn since),
/// or was never exported. No paint reading ⇒ nothing to record. Keeps
/// capture to once per completed turn — see [`AppState::sweep_usage_exports`].
pub(super) fn should_reexport(
    last_paint: Option<std::time::Instant>,
    last_export: Option<std::time::Instant>,
) -> bool {
    match last_paint {
        Some(paint) => last_export.is_none_or(|e| paint > e),
        None => false,
    }
}

/// A live usage checkpoint is useful only after a turn has settled.
/// Delete owns teardown and its final best-effort export, so ordinary
/// maintenance must never race it. A failed checkpoint is also held until
/// its retry deadline rather than being relaunched on every two-second beat.
pub(super) fn checkpoint_export_due(
    delete_requested: bool,
    activity: MissionActivity,
    last_paint: Option<std::time::Instant>,
    last_export: Option<std::time::Instant>,
    retry_after: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    !delete_requested
        && activity == MissionActivity::Waiting
        && retry_after.is_none_or(|deadline| now >= deadline)
        && should_reexport(last_paint, last_export)
}

/// Read-only discovery for sessions not owned by this app process.
pub(super) trait SessionCatalog: Send + Sync {
    fn live_tui_sessions(&self) -> Vec<String>;
    fn raw_log(&self, store: &Store, project: &str, session: &str) -> Option<PathBuf>;
}

pub(super) struct CoreSessionCatalog;

impl SessionCatalog for CoreSessionCatalog {
    fn live_tui_sessions(&self) -> Vec<String> {
        corpus_core::live_tui_sessions()
    }

    fn raw_log(&self, store: &Store, project: &str, session: &str) -> Option<PathBuf> {
        corpus_core::session_raw_log(store, project, session)
    }
}

pub(super) struct SessionMaintenance {
    /// Mission slug, exact tmux launch identity, OpenCode conversation id.
    pub(super) conversations: Vec<(String, String, String, String)>,
    pub(super) discovery_failure: Option<(String, String)>,
    pub(super) exported_tmux: Vec<String>,
    pub(super) export_failure: Option<(String, String)>,
    pub(super) warning: Option<String>,
}

impl AppState {
    /// Re-list the live corpus tmux sessions (the re-attach list shown
    /// when the app was relaunched over a surviving run).
    pub fn refresh_live_sessions(&mut self) {
        self.live_sessions_dirty = false;
        if let Some(jobs) = self.jobs.as_mut() {
            self.live_sessions_polled_at = Some(self.clock.monotonic_now());
            let catalog = self.session_catalog.clone();
            jobs.start(
                JobKind::SessionDiscovery,
                JobScope {
                    project: String::new(),
                    project_generation: 0,
                    corpus_revision: None,
                    run_id: None,
                },
                Duration::from_secs(10),
                move |_| Ok(AppJobOutput::LiveSessions(catalog.live_tui_sessions())),
            );
            return;
        }
        self.live_sessions = self.session_catalog.live_tui_sessions();
        self.live_sessions_polled_at = Some(self.clock.monotonic_now());
    }

    pub(super) fn apply_live_sessions(&mut self, sessions: Vec<String>) {
        let liveness_changed = self.live_sessions != sessions;
        self.live_sessions = sessions;
        self.live_sessions_polled_at = Some(self.clock.monotonic_now());
        self.session_lifecycle_stats.live_refreshes = self
            .session_lifecycle_stats
            .live_refreshes
            .saturating_add(1);
        if liveness_changed {
            self.schedule_session_reconciliation(true);
        }
    }

    /// The single owner of expensive session follow-up. A liveness edge runs
    /// immediately; raw-capture filesystem events use the same path; a slow
    /// timed backstop covers missed notifications. The cheap tmux listing is
    /// deliberately not itself a reason to rescan dispatch/store state.
    pub(super) fn schedule_session_reconciliation(&mut self, liveness_changed: bool) {
        self.session_reconciled_at = Some(self.clock.monotonic_now());
        self.session_reconcile_due_at = None;
        self.session_lifecycle_stats.reconciliation_passes = self
            .session_lifecycle_stats
            .reconciliation_passes
            .saturating_add(1);
        if liveness_changed {
            self.reconcile_mission_dispatches();
        }
        self.schedule_dispatch_deliveries();
        if let Some(project) = self.effective_project() {
            self.schedule_session_maintenance(&project);
        }
    }

    pub(super) fn schedule_session_maintenance(&mut self, project: &str) {
        let missions = self
            .trees
            .get(project)
            .map(|tree| tree.missions.clone())
            .unwrap_or_default();
        let live = self.live_sessions.clone();
        let now = self.clock.monotonic_now();
        let pending_conversations = missions
            .iter()
            .filter(|(_, mission)| mission.delete_requested.is_none())
            .filter(|(_, mission)| {
                mission.opencode_session.is_none() || mission.opencode_workspace.is_none()
            })
            .filter_map(|(slug, mission)| {
                Some((
                    slug.clone(),
                    mission.session.clone()?,
                    mission.opencode_session.clone(),
                    mission_workspace_candidates(&self.store, project, mission).ok()?,
                ))
            })
            .filter(|(_, tmux, _, _)| live.iter().any(|session| session == tmux))
            .filter(|(_, tmux, _, _)| {
                self.export_retry_after
                    .get(tmux)
                    .is_none_or(|deadline| now >= *deadline)
            })
            .collect::<Vec<_>>();
        let pending_exports = missions
            .iter()
            .filter_map(|(slug, mission)| {
                Some((
                    slug.clone(),
                    mission.delete_requested.is_some(),
                    mission.opencode_session.clone()?,
                    mission.session.clone()?,
                    mission.control.clone()?,
                    mission_workspace_dir(&self.store, project, mission).ok()?,
                ))
            })
            .filter(|(_, _, _, tmux, control, _)| {
                control.run_id == *tmux && live.iter().any(|session| session == tmux)
            })
            .filter(|(slug, deleting, _, tmux, _, _)| {
                checkpoint_export_due(
                    *deleting,
                    self.mission_activity(project, slug),
                    self.session_activity.get(tmux).copied(),
                    self.last_exported_at.get(tmux).copied(),
                    self.export_retry_after.get(tmux).copied(),
                    now,
                )
            })
            // Checkpoints are serialized deliberately. A second quiet
            // conversation waits for the next maintenance beat instead of
            // turning one job into an unbounded batch of CLI subprocesses.
            .take(1)
            .map(|(slug, _, conversation, tmux, control, workspace)| {
                let lease = self.session_operation_leases.claim(project, &slug);
                (slug, conversation, tmux, control, workspace, lease)
            })
            .collect::<Vec<_>>();
        if pending_conversations.is_empty() && pending_exports.is_empty() {
            return;
        }
        let scope = self.job_scope(project, None);
        let store = self.store.clone();
        let service = self.session_service.clone();
        let project_owned = project.to_string();
        let Some(jobs) = self.jobs.as_mut() else {
            return;
        };
        jobs.start(
            JobKind::SessionExport,
            scope,
            Duration::from_secs(30),
            move |_| {
                let mut conversations = Vec::new();
                let mut discovery_failure = None;
                let mut claimed = missions
                    .iter()
                    .filter_map(|(_, mission)| mission.opencode_session.clone())
                    .collect::<BTreeSet<_>>();
                for (slug, tmux, known_session, workspaces) in pending_conversations {
                    let Some(launched_at_ms) = launch_stamp_ms(&tmux) else {
                        continue;
                    };
                    let identity = match known_session {
                        Some(conversation) => service
                            .find_session_workspace(&workspaces, &conversation)
                            .map(|workspace| (conversation, workspace)),
                        None => service.find_for_launch_in_workspaces(
                            &workspaces,
                            launched_at_ms,
                            &claimed,
                        ),
                    };
                    match identity {
                        Ok((conversation, workspace)) => {
                            claimed.insert(conversation.clone());
                            conversations.push((slug, tmux, conversation, workspace));
                        }
                        Err(error) => {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            if error != "no OpenCode session found for this launch"
                                || now_ms.saturating_sub(launched_at_ms) >= 30_000
                            {
                                discovery_failure = Some((tmux, error));
                            }
                        }
                    }
                }
                let mut exported_tmux = Vec::new();
                let mut export_failure = None;
                for (slug, conversation, tmux, control, workspace, lease) in pending_exports {
                    let _ownership = lease
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    // Delete may have committed while this checkpoint waited
                    // for an earlier owner. Recheck the durable record under
                    // the lease so stale work cannot run after teardown.
                    let still_current = load_launchable_mission(&store, &project_owned, &slug)
                        .is_ok_and(|mission| {
                            mission.session.as_deref() == Some(tmux.as_str())
                                && mission.opencode_session.as_deref()
                                    == Some(conversation.as_str())
                        });
                    if !still_current {
                        continue;
                    }
                    let session = SessionRef {
                        id: conversation.clone(),
                        directory: workspace,
                    };
                    let exported = corpus_core::opencode_control_password(&store, &control.run_id)
                        .map_err(|error| error.to_string())
                        .and_then(|password| service.usage_snapshot(&control, &password, &session))
                        .and_then(|snapshot| {
                            store
                                .write_usage_snapshot(&project_owned, &snapshot)
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        });
                    match exported {
                        Ok(()) => exported_tmux.push(tmux),
                        Err(error) => export_failure = Some((tmux, error)),
                    }
                }
                Ok(AppJobOutput::SessionMaintenance(SessionMaintenance {
                    conversations,
                    discovery_failure,
                    exported_tmux,
                    export_failure,
                    warning: service.take_warning(),
                }))
            },
        );
    }

    /// Refresh liveness immediately after a relevant filesystem mutation and
    /// otherwise on a slow safety backstop. Each refresh launches `tmux
    /// list-sessions`, so a static live session must not imply a subprocess
    /// every repaint or every activity-dot transition.
    pub fn poll_live_sessions(&mut self) {
        let relevant = self.run_active()
            || self
                .trees
                .values()
                .any(|t| t.missions.iter().any(|(_, m)| m.session.is_some()));
        if !relevant {
            return;
        }
        let now = self.clock.monotonic_now();
        let due = self.live_sessions_dirty
            || self
                .live_sessions_polled_at
                .is_none_or(|t| now.saturating_duration_since(t) >= LIVE_SESSION_BACKSTOP);
        if due {
            self.refresh_live_sessions();
            if self.jobs.is_none() {
                self.reconcile_mission_dispatches();
                // Synchronous test/headless fallback retains the legacy
                // maintenance path. Production schedules listing first;
                // maintenance moves to its own scoped jobs below.
                if let Some(project) = self.effective_project() {
                    self.sweep_conversations(&project);
                    self.sweep_usage_exports(&project);
                }
            }
        }
        // Activity is a `stat` per live session — no subprocess, so it
        // polls faster: the dot should catch a turn starting, not lag it
        // by the tmux listing's throttle.
        let now = self.clock.monotonic_now();
        let activity_due = self.session_activity_polled_at.is_none_or(|t| {
            let elapsed = now.saturating_duration_since(t);
            elapsed >= ACTIVITY_BACKSTOP
                || self.session_activity_dirty && elapsed >= ACTIVITY_EVENT_MIN
        });
        if activity_due {
            self.refresh_session_activity();
            // Same beat: catch the live run's opencode session id as soon
            // as the TUI has created it (self-throttled, and a no-op once
            // the mission record has one).
            if self.jobs.is_none() {
                self.capture_opencode_session();
            }
        }
        if self.jobs.is_some() {
            let settled = self
                .session_reconcile_due_at
                .is_some_and(|deadline| now >= deadline);
            let backstop_due = self
                .session_reconciled_at
                .is_none_or(|at| now.saturating_duration_since(at) >= SESSION_RECONCILE_BACKSTOP);
            if settled || backstop_due {
                self.schedule_session_reconciliation(false);
            }
        }
    }

    /// Re-stat the raw capture of every mission session we know of, and
    /// record WHEN it last grew as an `Instant`. Storing the instant (not
    /// the age) means the reading keeps aging correctly between polls, so
    /// a 500 ms poll still gives a dot that goes still the moment output
    /// stops.
    pub(super) fn refresh_session_activity(&mut self) {
        let now = self.clock.monotonic_now();
        self.session_activity_polled_at = Some(now);
        self.session_activity_dirty = false;
        let sessions: Vec<(String, String)> = self
            .trees
            .iter()
            .flat_map(|(project, tree)| {
                tree.missions.iter().filter_map(move |(_, mission)| {
                    mission.session.clone().map(|s| (project.clone(), s))
                })
            })
            .collect();
        self.session_activity.clear();
        for (project, session) in sessions {
            let Some(log) = self
                .session_catalog
                .raw_log(&self.store, &project, &session)
            else {
                continue;
            };
            let Some(idle) = corpus_core::run_idle_secs(&log) else {
                continue;
            };
            let last_paint = now.checked_sub(Duration::from_secs(idle)).unwrap_or(now);
            self.session_activity.insert(session, last_paint);
        }
    }

    /// Poll each live controlled mission's own OpenCode server. The request
    /// runs off the UI thread; the exact mission/session/control identities
    /// are revalidated when results are applied.
    pub fn poll_session_statuses(&mut self) {
        let Some(project) = self.effective_project() else {
            return;
        };
        let now = self.clock.monotonic_now();
        let interval = if self.session_statuses.iter().any(|((p, _), observation)| {
            p == &project
                && observation
                    .status
                    .as_ref()
                    .is_some_and(|status| !matches!(status, OpenCodeSessionStatus::Idle))
        }) {
            SESSION_STATUS_BUSY_POLL
        } else {
            SESSION_STATUS_IDLE_POLL
        };
        if self
            .session_status_polled_at
            .is_some_and(|at| now.saturating_duration_since(at) < interval)
        {
            return;
        }

        let targets = self
            .trees
            .get(&project)
            .into_iter()
            .flat_map(|tree| tree.missions.iter())
            .filter_map(|(slug, mission)| {
                let tmux = mission.session.clone()?;
                if !self.live_sessions.iter().any(|live| live == &tmux) {
                    return None;
                }
                let control = mission.control.clone()?;
                if control.run_id != tmux {
                    return None;
                }
                let session = mission_session_ref(&self.store, &project, mission).ok()?;
                Some((slug.clone(), tmux, control, session))
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        self.session_status_polled_at = Some(now);
        let scope = self.job_scope(&project, None);
        let store = self.store.clone();
        let service = self.session_service.clone();
        let Some(jobs) = self.jobs.as_mut() else {
            return;
        };
        jobs.start(
            JobKind::SessionStatus,
            scope,
            Duration::from_secs(10),
            move |cancel| {
                let updates = std::thread::scope(|scope| {
                    let handles = targets
                        .into_iter()
                        .map(|(mission, run_id, control, session)| {
                            let store = store.clone();
                            let service = service.clone();
                            let cancel = cancel.clone();
                            scope.spawn(move || {
                                let result = if cancel.is_cancelled() {
                                    Err("session status poll cancelled".into())
                                } else {
                                    corpus_core::opencode_control_password(&store, &control.run_id)
                                        .map_err(|error| error.to_string())
                                        .and_then(|password| {
                                            service.session_status(&control, &password, &session)
                                        })
                                };
                                SessionStatusUpdate {
                                    mission,
                                    run_id,
                                    result,
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    handles
                        .into_iter()
                        .filter_map(|handle| handle.join().ok())
                        .collect()
                });
                Ok(AppJobOutput::SessionStatuses(updates))
            },
        );
    }

    pub(super) fn apply_session_status_updates(
        &mut self,
        project: &str,
        updates: Vec<SessionStatusUpdate>,
    ) {
        let now = self.clock.monotonic_now();
        let mut activity_changed = false;
        for update in updates {
            let still_current = self
                .trees
                .get(project)
                .into_iter()
                .flat_map(|tree| tree.missions.iter())
                .find(|(slug, _)| slug == &update.mission)
                .is_some_and(|(_, mission)| {
                    mission.session.as_deref() == Some(update.run_id.as_str())
                        && mission
                            .control
                            .as_ref()
                            .is_some_and(|control| control.run_id == update.run_id)
                });
            if !still_current {
                continue;
            }
            let key = (project.to_string(), update.mission);
            match update.result {
                Ok(status) => {
                    activity_changed |= self.session_statuses.get(&key).is_none_or(|previous| {
                        previous.run_id != update.run_id
                            || previous.status.as_ref() != Some(&status)
                            || previous.failed_at.is_some()
                    });
                    self.session_statuses.insert(
                        key,
                        MissionStatusObservation {
                            run_id: update.run_id,
                            status: Some(status),
                            observed_at: now,
                            failed_at: None,
                        },
                    );
                }
                Err(_) => {
                    let observation =
                        self.session_statuses
                            .entry(key)
                            .or_insert(MissionStatusObservation {
                                run_id: update.run_id.clone(),
                                status: None,
                                observed_at: now,
                                failed_at: Some(now),
                            });
                    if observation.run_id == update.run_id && observation.failed_at.is_none() {
                        observation.failed_at = Some(now);
                    }
                }
            }
        }
        if activity_changed {
            self.schedule_session_reconciliation(false);
        }
    }

    /// `None` means this is not a live controlled OpenCode mission and the
    /// legacy capture-age signal may be used. `Some(None)` means it is
    /// controlled but its authoritative status is currently unavailable.
    fn controlled_mission_status(
        &self,
        project: &str,
        slug: &str,
    ) -> Option<Option<&OpenCodeSessionStatus>> {
        let mission = self
            .trees
            .get(project)?
            .missions
            .iter()
            .find(|(candidate, _)| candidate == slug)?
            .1
            .clone();
        let tmux = mission.session?;
        let control = mission.control?;
        if control.run_id != tmux
            || mission.opencode_session.is_none()
            || mission.opencode_workspace.is_none()
            || !self.live_sessions.iter().any(|live| live == &tmux)
        {
            return None;
        }
        let observation = self.session_statuses.get(&(project.into(), slug.into()));
        if observation.is_none() {
            // During identity adoption and in headless coordinators there is
            // no authoritative reading yet. Preserve the legacy signal only
            // for this short bootstrap window; the first poll replaces it.
            return None;
        }
        Some(observation.and_then(|observation| {
            let now = self.clock.monotonic_now();
            let fresh = observation.failed_at.map_or_else(
                || now.saturating_duration_since(observation.observed_at) <= SESSION_STATUS_GRACE,
                |failed_at| now.saturating_duration_since(failed_at) <= SESSION_STATUS_GRACE,
            );
            (observation.run_id == tmux && observation.status.is_some() && fresh)
                .then(|| observation.status.as_ref())
                .flatten()
        }))
    }

    /// What the mission's status dot should say: `Idle` (nothing up),
    /// `Waiting` (session live, agent quiet), or `Working` (producing
    /// right now).
    ///
    /// A run is UP when the app-owned run is live and belongs to this
    /// mission (an owned handle in `Running`), or when the
    /// mission's recorded tmux session is alive on the server — which
    /// covers a relaunched app and sessions the app never owned.
    ///
    /// Controlled TUI missions use OpenCode's process-local status, which
    /// remains busy through quiet inference and tool calls. Raw capture age
    /// is retained only for legacy sessions without control identity. A
    /// piped headless run is one-shot by nature: while it is up, it is busy.
    pub fn mission_activity(&self, project: &str, slug: &str) -> MissionActivity {
        if let Some(status) = self.controlled_mission_status(project, slug) {
            return match status {
                Some(OpenCodeSessionStatus::Idle) => MissionActivity::Waiting,
                Some(OpenCodeSessionStatus::Busy | OpenCodeSessionStatus::Retrying { .. }) => {
                    MissionActivity::Working
                }
                // Unknown is conservatively non-idle so checkpoint/export
                // logic cannot mistake an observation failure for turn end.
                None => MissionActivity::Working,
            };
        }
        let owned = self.run_active() && self.run_belongs_to(project, slug);
        let session = self
            .trees
            .get(project)
            .and_then(|tree| tree.missions.iter().find(|(s, _)| s == slug))
            .and_then(|(_, mission)| mission.session.clone());
        let Some(session) = session else {
            // No tmux session: the only thing that can be up is an
            // app-owned piped run, which is busy for its whole life.
            return if owned {
                MissionActivity::Working
            } else {
                MissionActivity::Idle
            };
        };
        let live = self.live_sessions.iter().any(|l| l == &session)
            || self.live_run_session().as_deref() == Some(session.as_str())
            || owned;
        activity_for(
            self.clock.monotonic_now(),
            live,
            self.session_activity.get(&session).copied(),
        )
    }

    /// Collapse lifecycle, durable requests, and live activity into the
    /// shared static status language used throughout the operator UI.
    pub fn mission_display_state(&self, project: &str, slug: &str) -> MissionDisplayState {
        let queued = self
            .trees
            .get(project)
            .and_then(|tree| {
                tree.missions
                    .iter()
                    .find(|(candidate, _)| candidate == slug)
            })
            .is_some_and(|(_, mission)| mission.launch_requested.is_some());
        let phase = self.latest_run_phase(project, slug);
        let projected = mission_display_state_from(
            self.mission_activity(project, slug),
            &phase,
            queued,
            self.mission_delete_pending(project, slug),
        );
        if matches!(phase, RunPhase::Idle | RunPhase::Running) {
            match self.controlled_mission_status(project, slug) {
                Some(Some(OpenCodeSessionStatus::Retrying { .. })) => MissionDisplayState::Retrying,
                Some(None) => MissionDisplayState::Unavailable,
                _ => projected,
            }
        } else {
            projected
        }
    }

    pub fn mission_status_text(&self, project: &str, slug: &str) -> String {
        match self.controlled_mission_status(project, slug) {
            Some(Some(OpenCodeSessionStatus::Retrying {
                attempt, message, ..
            })) if !message.is_empty() => {
                format!("retrying (attempt {attempt}) · {message}")
            }
            Some(Some(OpenCodeSessionStatus::Retrying { attempt, .. })) => {
                format!("retrying (attempt {attempt})")
            }
            _ => self
                .mission_display_state(project, slug)
                .label()
                .to_string(),
        }
    }

    /// The only app-owned repaint clock. Background jobs, terminal output
    /// and chat events wake egui at delivery; this deadline exists solely
    /// while a run/session needs liveness polling or an activity dot needs
    /// animation. With no live producer it returns `None`, so an idle window
    /// schedules zero frames.
    pub fn live_repaint_after(&self) -> Option<Duration> {
        let busiest = self
            .trees
            .iter()
            .flat_map(|(project, tree)| {
                tree.missions
                    .iter()
                    .map(move |(slug, _)| self.mission_activity(project, slug))
            })
            .max_by_key(|activity| match activity {
                MissionActivity::Working => 2,
                MissionActivity::Waiting => 1,
                MissionActivity::Idle => 0,
            })
            .unwrap_or(MissionActivity::Idle);

        let activity_after = match busiest {
            // PTY/file/job producers wake egui when new data arrives. The
            // clock is only a bounded liveness fallback for an app-owned
            // process, not an animation timer.
            MissionActivity::Working if self.run_active() => Some(OWNED_RUN_REPAINT_BACKSTOP),
            MissionActivity::Working => Some(Duration::from_secs(2)),
            MissionActivity::Waiting => Some(Duration::from_secs(2)),
            MissionActivity::Idle if self.run_active() => Some(OWNED_RUN_REPAINT_BACKSTOP),
            // A just-discovered session may precede the mission cache that
            // names it. Keep the slow ownership beat until the cache lands.
            MissionActivity::Idle if !self.live_sessions.is_empty() => Some(Duration::from_secs(2)),
            MissionActivity::Idle => None,
        };
        let status_after = self.effective_project().and_then(|project| {
            self.trees.get(&project).and_then(|tree| {
                tree.missions
                    .iter()
                    .any(|(slug, _)| self.controlled_mission_status(&project, slug).is_some())
                    .then(|| {
                        let interval = if tree.missions.iter().any(|(slug, _)| {
                            matches!(
                                self.controlled_mission_status(&project, slug),
                                Some(Some(
                                    OpenCodeSessionStatus::Busy
                                        | OpenCodeSessionStatus::Retrying { .. }
                                ))
                            )
                        }) {
                            SESSION_STATUS_BUSY_POLL
                        } else {
                            SESSION_STATUS_IDLE_POLL
                        };
                        self.session_status_polled_at.map_or(Duration::ZERO, |at| {
                            interval.saturating_sub(
                                self.clock.monotonic_now().saturating_duration_since(at),
                            )
                        })
                    })
            })
        });
        match (activity_after, status_after) {
            (Some(activity), Some(status)) => Some(activity.min(status)),
            (Some(after), None) | (None, Some(after)) => Some(after),
            (None, None) => None,
        }
    }

    /// The attach argv for a discovered live session (re-attach after an
    /// app relaunch; None when tmux is gone).
    pub fn session_attach_command(session: &str) -> Option<Vec<String>> {
        corpus_core::tui_attach_command(session)
    }

    // --- mission bookkeeping (run attach + sidebar ops) ---

    /// The tmux session of the app-owned live run, if any.
    pub(crate) fn live_run_session(&self) -> Option<String> {
        self.live_pty_attach()
            .and_then(|argv| AppState::pty_attach_session(&argv))
    }
}
