//! Owned-run lifecycle state and operator-facing run projections.

mod coordinator;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use corpus_core::{Error, Mission, Project, RunLine, RunSession, StopOutcome, Store};

use super::{strip_ansi, AppState, RunExit, RunId, RunMeta, RunPhase, RunPhaseKind, MAX_RUN_LINES};

#[derive(Clone, Copy)]
pub(super) enum LaunchMode {
    AdoptFresh,
    DetachedFresh,
    Resume,
}

pub(super) struct LaunchReady {
    pub(super) session: Box<dyn ActiveRun>,
    pub(super) mode: LaunchMode,
    pub(super) notice: Option<String>,
    pub(super) environment_session: Option<String>,
}

pub(super) struct TeardownReady {
    pub(super) transcript: Option<String>,
    pub(super) error: Option<String>,
    pub(super) cleanup_complete: bool,
    pub(super) retained: Option<Box<dyn ActiveRun>>,
}

#[derive(Clone, Default)]
pub(super) struct RunCancellation(pub(super) crate::jobs::CancellationToken);

impl RunCancellation {
    pub(super) fn cancel(&self) {
        self.0.cancel();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// The process-owning half of a run. App state sees lifecycle facts, not
/// child-process or tmux implementation details.
pub(super) trait ActiveRun: Send {
    fn poll_line(&mut self) -> Option<RunLine>;
    fn try_exit_code(&mut self) -> Option<i32>;
    fn pty_attach_command(&self) -> Option<Vec<String>>;
    fn stop(&mut self) -> StopOutcome;
    fn opencode_session_id(&mut self, claimed: &BTreeSet<String>) -> Option<String>;
    fn launch_identity(&self) -> Option<String>;
    fn control_port(&self) -> Option<u16>;
    fn workspace_id(&self) -> Option<String>;
}

impl ActiveRun for RunSession {
    fn poll_line(&mut self) -> Option<RunLine> {
        RunSession::poll_line(self)
    }

    fn try_exit_code(&mut self) -> Option<i32> {
        RunSession::try_exit(self).map(|status| status.code().unwrap_or(1))
    }

    fn pty_attach_command(&self) -> Option<Vec<String>> {
        RunSession::pty_attach_command(self)
    }

    fn stop(&mut self) -> StopOutcome {
        RunSession::stop_detailed(self)
    }

    fn opencode_session_id(&mut self, claimed: &BTreeSet<String>) -> Option<String> {
        RunSession::opencode_session_id(self, claimed)
    }

    fn launch_identity(&self) -> Option<String> {
        RunSession::launch_identity(self)
    }

    fn control_port(&self) -> Option<u16> {
        RunSession::control_port(self)
    }

    fn workspace_id(&self) -> Option<String> {
        RunSession::workspace_id(self)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) trait RunBackend: Send + Sync {
    fn spawn(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error>;

    fn resume(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error>;

    fn prepare_source_pins(
        &self,
        store: &Store,
        project: &str,
        pins: &BTreeMap<String, String>,
        cancellation: &RunCancellation,
    ) -> Result<BTreeMap<String, String>, Error>;

    fn export_session(
        &self,
        project: &str,
        workspace: &str,
        opencode_session_id: &str,
    ) -> Result<PathBuf, Error>;
    fn kill_tmux_session(&self, session: &str) -> Result<(), Error>;
}

pub(super) struct CoreRunBackend;

impl RunBackend for CoreRunBackend {
    fn spawn(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error> {
        if cancellation.is_cancelled() {
            return Err(Error::Store("launch start cancelled".into()));
        }
        debug_assert_eq!(project, run_id.project);
        RunSession::spawn_mission_with_environment(
            run_id,
            agent,
            model,
            mission,
            source_pins_json,
            environment_session,
        )
        .map(|run| Box::new(run) as Box<dyn ActiveRun>)
    }

    fn resume(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error> {
        if cancellation.is_cancelled() {
            return Err(Error::Store("launch start cancelled".into()));
        }
        debug_assert_eq!(project, run_id.project);
        RunSession::resume_mission_with_environment(
            run_id,
            agent,
            model,
            opencode_session_id,
            source_pins_json,
            environment_session,
        )
        .map(|run| Box::new(run) as Box<dyn ActiveRun>)
    }

    fn prepare_source_pins(
        &self,
        store: &Store,
        project: &str,
        pins: &BTreeMap<String, String>,
        _cancellation: &RunCancellation,
    ) -> Result<BTreeMap<String, String>, Error> {
        corpus_core::prepare_source_pins(store, project, pins)
    }

    fn export_session(
        &self,
        project: &str,
        workspace: &str,
        opencode_session_id: &str,
    ) -> Result<PathBuf, Error> {
        corpus_core::export_session(project, workspace, opencode_session_id)
    }

    fn kill_tmux_session(&self, session: &str) -> Result<(), Error> {
        corpus_core::kill_tmux_session_checked(session)
    }
}

/// Host-side environment mutation used during launch. Keeping this seam next
/// to the process/session adapters prevents app tests from consulting whatever
/// immutable plugin version happens to be selected in the operator's home.
pub(super) trait EnvironmentRuntime: Send + Sync {
    fn open(
        &self,
        store: &Store,
        id: RunId,
        source_shas: BTreeMap<String, String>,
    ) -> Result<Option<corpus_core::EnvironmentSessionRecord>, Error>;
}

pub(super) struct CoreEnvironmentRuntime;

impl EnvironmentRuntime for CoreEnvironmentRuntime {
    fn open(
        &self,
        store: &Store,
        id: RunId,
        source_shas: BTreeMap<String, String>,
    ) -> Result<Option<corpus_core::EnvironmentSessionRecord>, Error> {
        corpus_core::open_environment_session(store, id, source_shas)
    }
}

#[cfg(test)]
pub(super) struct NoopEnvironmentRuntime;

#[cfg(test)]
impl EnvironmentRuntime for NoopEnvironmentRuntime {
    fn open(
        &self,
        _store: &Store,
        _id: RunId,
        _source_shas: BTreeMap<String, String>,
    ) -> Result<Option<corpus_core::EnvironmentSessionRecord>, Error> {
        Ok(None)
    }
}

pub(super) fn load_launchable_mission(
    store: &Store,
    project: &str,
    mission: &str,
) -> Result<Mission, Error> {
    let mission_record = store.load_mission(project, mission)?;
    if mission_record.delete_requested.is_some() {
        return Err(Error::Store(format!(
            "mission {project}/{mission} is pending deletion"
        )));
    }
    if Project::load(store, project)?.delete_requested.is_some() {
        return Err(Error::Store(format!(
            "project {project} is pending deletion"
        )));
    }
    if store
        .load_agent(project, &mission_record.agent)?
        .meta
        .delete_requested
        .is_some()
    {
        return Err(Error::Store(format!(
            "agent {project}/{} is pending deletion",
            mission_record.agent
        )));
    }
    Ok(mission_record)
}

pub(super) struct StopAttempt {
    pub(super) transcript: PathBuf,
    pub(super) error: Option<Error>,
    pub(super) cleanup_complete: bool,
}

impl RunPhase {
    pub(super) fn blocks_deletion(&self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Starting
                | Self::Running
                | Self::Stopping
                | Self::Exporting
                | Self::Failed {
                    cleanup_pending: true,
                    ..
                }
        )
    }

    pub(super) fn allows_delete_action(&self) -> bool {
        !matches!(
            self,
            Self::Preparing | Self::Starting | Self::Stopping | Self::Exporting
        )
    }
}

impl AppState {
    pub(super) fn project_has_inflight_run(&self, project: &str) -> bool {
        self.run_phases
            .iter()
            .any(|(id, phase)| id.project == project && phase.blocks_deletion())
    }

    // --- run launch ---

    /// Whether a run is currently live.
    pub fn run_active(&self) -> bool {
        self.run.is_some()
    }

    /// Materialize the agent and spawn the mission on the project
    /// scope. Runs OVERLAP: the caller backgrounds the live one first
    /// (`background_active_run`), and the handle this adopts replaces it.
    /// `source_pins_json` is the resolved `repo -> sha` map exported to
    /// the run (None = the plugin's default pins).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn launch(
        &mut self,
        run_id: RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<(), Error> {
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        let cancellation = self
            .run_cancellations
            .entry(run_id.clone())
            .or_default()
            .clone();
        // Fail loudly on an unknown agent, then materialize the WHOLE
        // project: the agent list opencode shows is project-scoped.
        let session = match (|| {
            self.store.load_agent(project, agent)?;
            self.store.render_project_agents(project)?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch start cancelled".into()));
            }
            self.run_backend.spawn(
                &run_id,
                project,
                agent,
                model,
                mission,
                source_pins_json,
                environment_session,
                &cancellation,
            )
        })() {
            Ok(session) => session,
            Err(error) => {
                self.run_cancellations.remove(&run_id);
                if cancellation.is_cancelled() {
                    self.finish_run(&run_id);
                } else {
                    self.fail_run(&run_id, RunPhaseKind::Starting, &error, true, false);
                }
                return Err(error);
            }
        };
        if let Err(error) = self.refuse_pending_mission_delete(project, &run_id.mission) {
            self.run_cancellations.remove(&run_id);
            return Err(self.reject_unadopted_run(&run_id, session, environment_session, error));
        }
        self.run_cancellations.remove(&run_id);
        self.adopt_run(session, run_id);
        Ok(())
    }

    /// Take ownership of a freshly spawned run and reset the per-run
    /// bookkeeping (attach argv, drained lines, terminal status). Shared
    /// by `launch` and `resume_mission` so a resumed run is wired exactly
    /// like a fresh one.
    pub(super) fn adopt_run(&mut self, session: Box<dyn ActiveRun>, run_id: RunId) {
        let pty_attach = session.pty_attach_command();
        self.run = Some(session);
        self.run_meta = Some(RunMeta { pty_attach });
        self.owned_run_id = Some(run_id.clone());
        self.run_phases.insert(run_id, RunPhase::Running);
        self.run_lines.clear();
    }

    pub(super) fn reject_unadopted_run(
        &mut self,
        run_id: &RunId,
        mut session: Box<dyn ActiveRun>,
        environment_session: Option<&str>,
        reason: Error,
    ) -> Error {
        self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
        let stopped = session.stop();
        let transcript = stopped.transcript.display().to_string();
        let mut cleanup_errors = stopped.cleanup_errors;
        if let Some(key) = environment_session {
            match Project::load(&self.store, &run_id.project) {
                Ok(project) => {
                    if let Err(error) = corpus_core::close_environment_session_key(
                        &self.store,
                        &project.plugin,
                        key,
                    ) {
                        cleanup_errors.push(format!("environment cleanup failed: {error}"));
                    }
                }
                Err(error) => {
                    cleanup_errors.push(format!("cannot resolve environment for cleanup: {error}"))
                }
            }
        }
        let mut detail = reason.to_string();
        if let Some(error) = stopped.export_error {
            detail.push_str(&format!("; final transcript export failed: {error}"));
        }
        if cleanup_errors.is_empty() {
            let error = Error::Store(format!(
                "{detail}; spawned run was stopped; transcript: {transcript}"
            ));
            self.fail_run(run_id, RunPhaseKind::Running, &error, false, false);
            error
        } else {
            let error = Error::Store(format!(
                "{detail}; spawned run cleanup failed: {}",
                cleanup_errors.join("; ")
            ));
            self.fail_run(run_id, RunPhaseKind::Stopping, &error, true, true);
            error
        }
    }

    pub(super) fn fail_run(
        &mut self,
        run_id: &RunId,
        at: RunPhaseKind,
        error: &Error,
        recoverable: bool,
        cleanup_pending: bool,
    ) {
        self.run_phases.insert(
            run_id.clone(),
            RunPhase::Failed {
                at,
                message: error.to_string(),
                recoverable,
                cleanup_pending,
            },
        );
    }

    pub(super) fn finish_run(&mut self, run_id: &RunId) {
        self.run_phases.remove(run_id);
    }

    pub fn run_phase(&self, run_id: &RunId) -> RunPhase {
        self.run_phases
            .get(run_id)
            .cloned()
            .unwrap_or(RunPhase::Idle)
    }

    pub fn latest_run_phase(&self, project: &str, mission: &str) -> RunPhase {
        self.run_phases
            .iter()
            .filter(|(id, _)| id.project == project && id.mission == mission)
            .max_by_key(|(id, _)| id.generation)
            .map(|(_, phase)| phase.clone())
            .unwrap_or(RunPhase::Idle)
    }

    /// Whether this mission already owns an operation that must finish or be
    /// cancelled before another Launch/Resume can start. This is enforced in
    /// state as well as reflected by disabled UI actions.
    pub fn mission_run_inflight(&self, project: &str, mission: &str) -> bool {
        self.latest_run_phase(project, mission).blocks_deletion()
    }

    /// User-facing Delete availability. Preparation and an active teardown
    /// are indivisible background operations, so a second Delete is held
    /// until they settle. Running and failed-cleanup states deliberately stay
    /// actionable: Delete is the command that starts or retries teardown.
    pub fn mission_delete_available(&self, project: &str, mission: &str) -> bool {
        self.latest_run_phase(project, mission)
            .allows_delete_action()
    }

    /// Whether Delete has committed to tearing this mission down in the
    /// background. Views keep the mission selected and render a stable busy
    /// pane until teardown and record removal complete.
    pub fn mission_delete_pending(&self, project: &str, mission: &str) -> bool {
        self.pending_mission_deletes
            .contains(&(project.to_string(), mission.to_string()))
            || (self.missions_project.as_deref() == Some(project)
                && self
                    .missions
                    .iter()
                    .find(|(slug, _)| slug == mission)
                    .is_some_and(|(_, record)| record.delete_requested.is_some()))
            || self
                .trees
                .get(project)
                .and_then(|tree| tree.missions.iter().find(|(slug, _)| slug == mission))
                .is_some_and(|(_, record)| record.delete_requested.is_some())
    }

    /// A mission may outlive the app between environment creation and agent
    /// spawn. Its persisted key remains the operator's cleanup handle even
    /// when no tmux session was ever created.
    pub fn mission_environment_needs_cleanup(&self, project: &str, mission: &str) -> bool {
        let Ok(mission) = self.store.load_mission(project, mission) else {
            return false;
        };
        let Some(key) = mission.environment_session.as_deref() else {
            return false;
        };
        let Ok(project) = corpus_core::Project::load(&self.store, project) else {
            return true;
        };
        self.store
            .load_environment_session_key(&project.plugin, key)
            .map(|record| record.state != corpus_core::EnvironmentSessionState::Closed)
            .unwrap_or(true)
    }

    pub(super) fn refuse_pending_mission_delete(
        &self,
        project: &str,
        mission: &str,
    ) -> Result<(), Error> {
        load_launchable_mission(&self.store, project, mission).map(drop)
    }

    pub(super) fn refuse_duplicate_mission_run(
        &self,
        project: &str,
        mission: &str,
    ) -> Result<(), Error> {
        // This is an early concurrency guard, before a run generation exists.
        // Preserve the launch path's established error accounting for missing
        // or unreadable missions: `prepare_launch` will record those failures.
        // A readable durable delete marker, however, can be refused without
        // allocating a generation or starting any preparation.
        if self.store.load_mission(project, mission).is_ok() {
            self.refuse_pending_mission_delete(project, mission)?;
        }
        if self.mission_run_inflight(project, mission) {
            Err(Error::Store(
                "this mission already has a run operation in progress".into(),
            ))
        } else if self.mission_environment_needs_cleanup(project, mission) {
            Err(Error::Store(
                "this mission has an environment requiring cleanup; stop it before launching"
                    .into(),
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn next_run_id(&mut self, project: &str, mission: &str) -> RunId {
        let generation = self
            .run_generations
            .entry((project.to_string(), mission.to_string()))
            .or_default();
        *generation += 1;
        RunId {
            project: project.to_string(),
            mission: mission.to_string(),
            generation: *generation,
        }
    }

    pub fn run_belongs_to(&self, project: &str, mission: &str) -> bool {
        self.owned_run_id
            .as_ref()
            .is_some_and(|id| id.project == project && id.mission == mission)
    }

    /// Drain any new transcript lines; mark the run finished the moment it
    /// exits. Called from the app-level update path (so an exit is noticed on
    /// any screen); live producer deadlines own the next poll.
    pub fn poll_run(&mut self) {
        let Some(mut session) = self.run.take() else {
            return;
        };
        while let Some(line) = session.poll_line() {
            // egui_term consumes the PTY directly. Keeping the parallel raw
            // capture here only duplicated an unbounded stream in memory.
            if self
                .run_meta
                .as_ref()
                .is_some_and(|meta| meta.pty_attach.is_none())
            {
                self.run_lines.push(RunLine {
                    stderr: line.stderr,
                    text: strip_ansi(&line.text),
                });
            }
        }
        if self.run_lines.len() > MAX_RUN_LINES {
            self.run_lines.drain(..self.run_lines.len() - MAX_RUN_LINES);
        }
        if let Some(code) = session.try_exit_code() {
            // Queue the report NOW: ownership still names the mission, and
            // once the handle is gone the pane going quiet is otherwise the
            // only evidence this run existed.
            self.run_exit = Some(RunExit {
                mission: self
                    .owned_run_id
                    .as_ref()
                    .map(|id| self.mission_display_label(&id.project, &id.mission)),
                code,
            });
            if let Some(run_id) = self.owned_run_id.take() {
                let completion = if code == 0 {
                    corpus_core::MissionCompletion::Completed {
                        at: self.clock.unix_seconds(),
                        artifacts: Vec::new(),
                    }
                } else {
                    corpus_core::MissionCompletion::UnexpectedExit {
                        at: self.clock.unix_seconds(),
                    }
                };
                let _ =
                    self.record_dispatch_completion(&run_id.project, &run_id.mission, completion);
                self.finish_run(&run_id);
            }
            return;
        }
        self.run = Some(session);
    }

    /// Take the pending end-of-run report, if one is waiting. Drained by
    /// the app loop once per exit.
    pub fn take_run_exit(&mut self) -> Option<RunExit> {
        self.run_exit.take()
    }

    /// Operator-initiated stop: attempt transcript-of-record export, then
    /// always attempt cleanup. Returns the durable transcript path (the
    /// exported JSON when it lands, else the raw/.log fallback) — the
    /// caller is what reports it, so nothing is stored here.
    pub(super) fn stop_run(&mut self) -> Option<StopAttempt> {
        let mut session = self.run.take()?;
        let run_id = self.owned_run_id.take();
        if let Some(id) = &run_id {
            self.run_phases.insert(id.clone(), RunPhase::Stopping);
        }
        let outcome = session.stop();
        let cleanup_complete = outcome.cleanup_errors.is_empty();
        let mut errors = Vec::new();
        if let Some(error) = outcome.export_error {
            errors.push(error);
        }
        errors.extend(outcome.cleanup_errors);
        let error = if errors.is_empty() {
            None
        } else {
            Some(Error::Store(format!(
                "stop completed with errors (transcript: {}): {}",
                outcome.transcript.display(),
                errors.join("; ")
            )))
        };
        if let Some(id) = &run_id {
            if cleanup_complete {
                self.finish_run(id);
                if let Some(error) = &error {
                    self.fail_run(id, RunPhaseKind::Exporting, error, true, false);
                }
            } else {
                self.run = Some(session);
                self.owned_run_id = Some(id.clone());
                self.fail_run(
                    id,
                    RunPhaseKind::Stopping,
                    error.as_ref().unwrap(),
                    true,
                    true,
                );
            }
        }
        Some(StopAttempt {
            transcript: outcome.transcript,
            error,
            cleanup_complete,
        })
    }

    /// The embedded-PTY attach argv of the live run: Some for a tmux TUI,
    /// None for the piped fallback or no run. The mission screen shows the
    /// transcript tail when no attachable pane exists.
    pub fn live_pty_attach(&self) -> Option<Vec<String>> {
        if !self.run_active() {
            return None;
        }
        self.run_meta.as_ref()?.pty_attach.clone()
    }

    /// The tmux session name embedded in a pty attach argv, if any.
    pub fn pty_attach_session(argv: &[String]) -> Option<String> {
        argv.windows(2).find(|w| w[0] == "-t").map(|w| w[1].clone())
    }
}
