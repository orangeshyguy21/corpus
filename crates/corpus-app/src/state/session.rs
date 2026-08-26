//! External session discovery, maintenance, activity, and repaint projection.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use corpus_core::Store;

use super::{
    launch_stamp_ms, load_launchable_mission, AppJobOutput, AppState, MissionActivity,
    MissionDisplayState, RunPhase, SessionRef, ACTIVITY_BACKSTOP, ACTIVITY_EVENT_MIN,
    LIVE_SESSION_BACKSTOP, OWNED_RUN_REPAINT_BACKSTOP, SESSION_RECONCILE_BACKSTOP,
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

/// The status dot's decision from the app's aged in-memory reading: turns a
/// `last_paint` Instant into idle-seconds and defers to the shared core
/// rule (`corpus_core::activity_from_idle`). The app keeps its own polled
/// cache (statting per frame would be far too much I/O) — only the rule and
/// the window are shared.
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
    pub(super) conversations: Vec<(String, String, String)>,
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
        let pending_conversations = missions
            .iter()
            .filter(|(_, mission)| mission.delete_requested.is_none())
            .filter(|(_, mission)| mission.opencode_session.is_none())
            .filter_map(|(slug, mission)| Some((slug.clone(), mission.session.clone()?)))
            .filter(|(_, tmux)| live.iter().any(|session| session == tmux))
            .collect::<Vec<_>>();
        let now = self.clock.monotonic_now();
        let pending_exports = missions
            .iter()
            .filter_map(|(slug, mission)| {
                Some((
                    slug.clone(),
                    mission.delete_requested.is_some(),
                    mission.opencode_session.clone()?,
                    mission.session.clone()?,
                    mission.control.clone()?,
                ))
            })
            .filter(|(_, _, _, tmux, control)| {
                control.run_id == *tmux && live.iter().any(|session| session == tmux)
            })
            .filter(|(slug, deleting, _, tmux, _)| {
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
            .map(|(slug, _, conversation, tmux, control)| {
                let lease = self.session_operation_leases.claim(project, &slug);
                (slug, conversation, tmux, control, lease)
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
                let mut claimed = missions
                    .iter()
                    .filter_map(|(_, mission)| mission.opencode_session.clone())
                    .collect::<BTreeSet<_>>();
                for (slug, tmux) in pending_conversations {
                    let Some(launched_at_ms) = launch_stamp_ms(&tmux) else {
                        continue;
                    };
                    if let Ok(conversation) = service.find_for_launch(
                        &store.project_run_dir(&project_owned),
                        launched_at_ms,
                        &claimed,
                    ) {
                        claimed.insert(conversation.clone());
                        conversations.push((slug, tmux, conversation));
                    }
                }
                let mut exported_tmux = Vec::new();
                let mut export_failure = None;
                for (slug, conversation, tmux, control, lease) in pending_exports {
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
                        directory: store.project_run_dir(&project_owned),
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

    /// What the mission's status dot should say: `Idle` (nothing up),
    /// `Waiting` (session live, agent quiet), or `Working` (producing
    /// right now).
    ///
    /// A run is UP when the app-owned run is live and belongs to this
    /// mission (an owned handle in `Running`), or when the
    /// mission's recorded tmux session is alive on the server — which
    /// covers a relaunched app and sessions the app never owned.
    ///
    /// The busy signal is the run's raw capture: everything the TUI
    /// paints flows through `tmux pipe-pane` into it, and a TUI waiting
    /// at its prompt paints nothing — so a capture that grew within
    /// `WORKING_WINDOW` means the agent is mid-turn. A piped headless run
    /// has no TUI to watch and is one-shot by nature: while it is up, it
    /// IS working.
    pub fn mission_activity(&self, project: &str, slug: &str) -> MissionActivity {
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
        mission_display_state_from(
            self.mission_activity(project, slug),
            &self.latest_run_phase(project, slug),
            queued,
            self.mission_delete_pending(project, slug),
        )
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

        match busiest {
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
