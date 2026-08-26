//! Asynchronous launch, session maintenance, and teardown coordination.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use corpus_core::{Error, Mission, Project};

use super::super::session::should_reexport;
use super::super::{
    launch_stamp_ms, mission_label, AppJobOutput, AppState, LaunchNotice, MissionActivity, RunId,
    RunPhase, RunPhaseKind, SessionRef, StopMissionResult,
};
use super::{load_launchable_mission, LaunchMode, LaunchReady, RunCancellation, TeardownReady};
use crate::jobs::JobKind;
use crate::observability::{LifecycleEvent, LifecycleOperation};

impl AppState {
    pub fn launch_mission(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        self.refuse_duplicate_mission_run(project, slug)?;
        if self.jobs.is_some() {
            return self.schedule_launch(project, slug, LaunchMode::AdoptFresh);
        }
        let run_id = self.next_run_id(project, slug);
        let (record, pins_json) = self.prepare_launch(&run_id)?;
        let prompt = self.mission_kickoff_prompt(project, slug);
        self.refuse_pending_mission_delete(project, slug)?;
        if let Err(error) = self.background_active_run() {
            self.run_cancellations.remove(&run_id);
            self.fail_run(&run_id, RunPhaseKind::Preparing, &error, true, false);
            return Err(error);
        }
        let model = self.agent_default_model(project, &record.agent);
        self.launch(
            run_id.clone(),
            project,
            &record.agent,
            model.as_deref(),
            &prompt,
            pins_json.as_deref(),
            record.environment_session.as_deref(),
        )?;
        let session = self.live_run_session();
        let control_port = self.run.as_ref().and_then(|run| run.control_port());
        let child_run_id = self.run.as_ref().and_then(|run| run.launch_identity());
        if let Err(error) = self.bind_fresh_run(project, slug, session, child_run_id, control_port)
        {
            return Err(self.cleanup_failed_adoption(&run_id, error));
        }
        self.refresh_missions(project);
        Ok(())
    }

    /// The kickoff prompt for a mission launch: its brief, trimmed. Empty
    /// (a briefless mission, e.g. the sidebar `+`) means a bare TUI at an
    /// empty prompt. A brief read failure is not fatal — a launch with no
    /// prompt still runs — so it degrades to empty rather than refusing.
    fn mission_kickoff_prompt(&self, project: &str, slug: &str) -> String {
        self.store
            .mission_brief(project, slug)
            .map(|b| b.trim().to_string())
            .unwrap_or_default()
    }

    /// Spawn a mission's run in the BACKGROUND — a full opencode TUI in a
    /// detached tmux session, kicked off with the brief — WITHOUT adopting
    /// it as the app-owned run. This is the curator's autonomous launch:
    /// the session is real and watchable (the operator selects the mission
    /// to attach the pane and interact), but it does not seize the pane
    /// from whatever the operator is already watching.
    ///
    /// The tmux session name is recorded on the mission record before the
    /// handle is dropped, so the app's own `refresh_live_sessions` and
    /// `sweep_conversations` discovery adopts it exactly like any run that
    /// outlived the app — attach, activity dot, and eventual export all follow
    /// from the recorded session. A no-tmux fallback cannot be backgrounded
    /// (the piped child lives on the handle), so there it adopts the run rather
    /// than orphaning it.
    pub fn launch_mission_detached(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        self.refuse_duplicate_mission_run(project, slug)?;
        if self.jobs.is_some() {
            return self.schedule_launch(project, slug, LaunchMode::DetachedFresh);
        }
        let run_id = self.next_run_id(project, slug);
        let (record, pins_json) = self.prepare_launch(&run_id)?;
        let prompt = self.mission_kickoff_prompt(project, slug);
        let model = self.agent_default_model(project, &record.agent);
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        let cancellation = self
            .run_cancellations
            .get(&run_id)
            .cloned()
            .unwrap_or_default();
        // Same materialization as an adopted launch: the run's agent set is
        // this project's, rendered fresh.
        let mut session = match (|| {
            self.refuse_pending_mission_delete(project, slug)?;
            self.store.load_agent(project, &record.agent)?;
            self.store.render_project_agents(project)?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch start cancelled".into()));
            }
            self.run_backend.spawn(
                &run_id,
                project,
                &record.agent,
                model.as_deref(),
                &prompt,
                pins_json.as_deref(),
                record.environment_session.as_deref(),
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
        if let Err(error) = self.refuse_pending_mission_delete(project, slug) {
            self.run_cancellations.remove(&run_id);
            return Err(self.reject_unadopted_run(
                &run_id,
                session,
                record.environment_session.as_deref(),
                error,
            ));
        }
        self.run_cancellations.remove(&run_id);
        let control_port = session.control_port();
        let child_run_id = session.launch_identity();
        let tmux = session
            .pty_attach_command()
            .and_then(|argv| AppState::pty_attach_session(&argv));
        match tmux {
            Some(name) => {
                // A detached TUI: record the session and let go. Discovery
                // takes it from here.
                if let Err(error) = self.bind_fresh_run(
                    project,
                    slug,
                    Some(name),
                    child_run_id.clone(),
                    control_port,
                ) {
                    self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
                    let transcript = session.stop();
                    let combined = Error::Store(format!(
                        "{error}; spawned run stop transcript: {}; cleanup errors: {}",
                        transcript.transcript.display(),
                        transcript.cleanup_errors.join("; ")
                    ));
                    self.fail_run(&run_id, RunPhaseKind::Running, &combined, false, false);
                    return Err(combined);
                }
                drop(session);
                self.finish_run(&run_id);
            }
            None => {
                // Piped fallback: the child lives on the handle, so it
                // must be adopted or it leaks. This does take the pane —
                // there is no detached session to hand off.
                if let Err(error) = self.background_active_run() {
                    let cleanup = session.stop();
                    let combined = Error::Store(format!(
                        "could not background the current run: {error}; new run cleanup transcript: {}; cleanup errors: {}",
                        cleanup.transcript.display(),
                        cleanup.cleanup_errors.join("; ")
                    ));
                    self.fail_run(&run_id, RunPhaseKind::Starting, &combined, true, false);
                    return Err(combined);
                }
                self.adopt_run(session, run_id.clone());
                if let Err(error) =
                    self.bind_fresh_run(project, slug, None, child_run_id, control_port)
                {
                    return Err(self.cleanup_failed_adoption(&run_id, error));
                }
            }
        }
        self.refresh_live_sessions();
        self.refresh_missions(project);
        Ok(())
    }

    /// Re-open a mission's recorded opencode conversation in a fresh TUI
    /// (`opencode --session <id>`), so an old mission whose tmux session
    /// died is steerable again with its history intact. Same rule as
    /// `launch_mission`: a live run is backgrounded, not stopped.
    pub fn resume_mission(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        self.refuse_duplicate_mission_run(project, slug)?;
        if self.jobs.is_some() {
            return self.schedule_launch(project, slug, LaunchMode::Resume);
        }
        let run_id = self.next_run_id(project, slug);
        let (record, pins_json) = self.prepare_launch(&run_id)?;
        let id = match record.opencode_session.clone() {
            Some(id) => id,
            None => {
                let error = Error::Store(
                    "no opencode session recorded for this mission — nothing to resume".into(),
                );
                self.run_cancellations.remove(&run_id);
                self.fail_run(&run_id, RunPhaseKind::Preparing, &error, true, false);
                return Err(error);
            }
        };
        self.refuse_pending_mission_delete(project, slug)?;
        if let Err(error) = self.background_active_run() {
            self.run_cancellations.remove(&run_id);
            self.fail_run(&run_id, RunPhaseKind::Preparing, &error, true, false);
            return Err(error);
        }
        // Same materialization as a launch: the resumed conversation runs
        // against this project's agent set.
        let model = self.agent_default_model(project, &record.agent);
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        let cancellation = self
            .run_cancellations
            .get(&run_id)
            .cloned()
            .unwrap_or_default();
        let run = match (|| {
            self.refuse_pending_mission_delete(project, slug)?;
            self.store.load_agent(project, &record.agent)?;
            self.store.render_project_agents(project)?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch start cancelled".into()));
            }
            self.run_backend.resume(
                &run_id,
                project,
                &record.agent,
                model.as_deref(),
                &id,
                pins_json.as_deref(),
                record.environment_session.as_deref(),
                &cancellation,
            )
        })() {
            Ok(run) => run,
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
        if let Err(error) = self.refuse_pending_mission_delete(project, slug) {
            self.run_cancellations.remove(&run_id);
            return Err(self.reject_unadopted_run(
                &run_id,
                run,
                record.environment_session.as_deref(),
                error,
            ));
        }
        self.run_cancellations.remove(&run_id);
        let resumed_run_id = run.launch_identity();
        let control_port = run.control_port();
        self.adopt_run(run, run_id.clone());
        if let Some(session) = self.live_run_session() {
            if let Err(error) =
                self.bind_resumed_run(project, slug, Some(session), resumed_run_id, control_port)
            {
                return Err(self.cleanup_failed_adoption(&run_id, error));
            }
        }
        self.refresh_missions(project);
        Ok(())
    }

    fn schedule_launch(
        &mut self,
        project: &str,
        slug: &str,
        mode: LaunchMode,
    ) -> Result<(), Error> {
        let run_id = self.next_run_id(project, slug);
        let environment_id = run_id.clone();
        self.run_phases.insert(run_id.clone(), RunPhase::Preparing);
        let scope = self.job_scope(project, Some(run_id.clone()));
        let store = self.store.clone();
        let backend = self.run_backend.clone();
        let environment_runtime = self.environment_runtime.clone();
        let project = project.to_string();
        let slug = slug.to_string();
        let jobs = self.jobs.as_mut().expect("installed above");
        jobs.start(
            JobKind::LaunchPreparation,
            scope,
            Duration::from_secs(120),
            move |job_cancellation| {
                let cancellation = RunCancellation(job_cancellation);
                if cancellation.is_cancelled() {
                    return Err("launch preparation cancelled".into());
                }
                let mut mission = load_launchable_mission(&store, &project, &slug)
                    .map_err(|error| error.to_string())?;
                let mut effective_pins = corpus_observe::project_source_pins(&store, &project)
                    .map_err(|error| error.to_string())?;
                effective_pins.extend(mission.pins.clone());
                mission.pins = effective_pins;
                let prepared = backend
                    .prepare_source_pins(&store, &project, &mission.pins, &cancellation)
                    .map_err(|error| error.to_string())?;
                if cancellation.is_cancelled() {
                    return Err("launch preparation cancelled".into());
                }
                load_launchable_mission(&store, &project, &slug)
                    .map_err(|error| error.to_string())?;
                let pins_json = if prepared.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&prepared).map_err(|error| error.to_string())?)
                };
                mission.environment_session = Some(environment_id.storage_key());
                store
                    .update_mission(&project, &slug, &mission)
                    .map_err(|error| error.to_string())?;
                if environment_runtime
                    .open(&store, environment_id.clone(), prepared.clone())
                    .map_err(|error| error.to_string())?
                    .is_none()
                {
                    mission.environment_session = None;
                    store
                        .update_mission(&project, &slug, &mission)
                        .map_err(|error| error.to_string())?;
                }
                if let Err(error) = load_launchable_mission(&store, &project, &slug) {
                    if let Some(key) = mission.environment_session.as_deref() {
                        if let Ok(project_record) = corpus_core::Project::load(&store, &project) {
                            let _ = corpus_core::close_environment_session_key(
                                &store,
                                &project_record.plugin,
                                key,
                            );
                        }
                    }
                    return Err(error.to_string());
                }
                store
                    .load_agent(&project, &mission.agent)
                    .map_err(|error| error.to_string())?;
                store
                    .render_project_agents(&project)
                    .map_err(|error| error.to_string())?;
                let final_guard = if cancellation.is_cancelled() {
                    Err("launch preparation cancelled".to_string())
                } else {
                    load_launchable_mission(&store, &project, &slug)
                        .map(drop)
                        .map_err(|error| error.to_string())
                };
                if let Err(error) = final_guard {
                    if let Some(key) = mission.environment_session.as_deref() {
                        if let Ok(project_record) = corpus_core::Project::load(&store, &project) {
                            let _ = corpus_core::close_environment_session_key(
                                &store,
                                &project_record.plugin,
                                key,
                            );
                        }
                    }
                    return Err(error);
                }
                let model = corpus_core::agent_default_model(&store, &project, &mission.agent);
                let session_result = match mode {
                    LaunchMode::Resume => {
                        let conversation =
                            mission.opencode_session.as_deref().ok_or_else(|| {
                                "no opencode session recorded for this mission — nothing to resume"
                                    .to_string()
                            })?;
                        backend.resume(
                            &run_id,
                            &project,
                            &mission.agent,
                            model.as_deref(),
                            conversation,
                            pins_json.as_deref(),
                            mission.environment_session.as_deref(),
                            &cancellation,
                        )
                    }
                    LaunchMode::AdoptFresh | LaunchMode::DetachedFresh => {
                        let prompt = store
                            .mission_brief(&project, &slug)
                            .map(|brief| brief.trim().to_string())
                            .unwrap_or_default();
                        backend.spawn(
                            &run_id,
                            &project,
                            &mission.agent,
                            model.as_deref(),
                            &prompt,
                            pins_json.as_deref(),
                            mission.environment_session.as_deref(),
                            &cancellation,
                        )
                    }
                };
                let session = match session_result {
                    Ok(session) => session,
                    Err(error) => {
                        if let Some(key) = mission.environment_session.as_deref() {
                            if let Ok(project_record) = corpus_core::Project::load(&store, &project)
                            {
                                let _ = corpus_core::close_environment_session_key(
                                    &store,
                                    &project_record.plugin,
                                    key,
                                );
                            }
                        }
                        return Err(error.to_string());
                    }
                };
                let notice = matches!(mode, LaunchMode::DetachedFresh)
                    .then(|| mission_label(mission.name.as_deref(), &slug));
                Ok(AppJobOutput::LaunchReady(LaunchReady {
                    session,
                    mode,
                    notice,
                    environment_session: mission.environment_session.clone(),
                }))
            },
        );
        Ok(())
    }

    pub(in crate::state) fn apply_launch_ready(
        &mut self,
        run_id: &RunId,
        ready: LaunchReady,
    ) -> Result<(), Error> {
        let started = Instant::now();
        let result = self.apply_launch_ready_inner(run_id, ready);
        let retryable = matches!(
            self.run_phase(run_id),
            RunPhase::Failed {
                recoverable: true,
                ..
            }
        );
        LifecycleEvent::new(
            run_id,
            LifecycleOperation::LaunchAdoption,
            started.elapsed(),
            retryable,
        )
        .emit_result(&result);
        result
    }

    fn apply_launch_ready_inner(
        &mut self,
        run_id: &RunId,
        mut ready: LaunchReady,
    ) -> Result<(), Error> {
        let mission = load_launchable_mission(&self.store, &run_id.project, &run_id.mission);
        let rejection = match &mission {
            Ok(_) => None,
            Err(error) => Some(format!(
                "mission {}/{} is no longer launchable before adoption: {error}",
                run_id.project, run_id.mission
            )),
        };
        if let Some(rejection) = rejection {
            self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
            let stopped = ready.session.stop();
            let mut cleanup_errors = stopped.cleanup_errors;
            let environment_session = mission
                .ok()
                .and_then(|mission| mission.environment_session)
                .or_else(|| ready.environment_session.clone());
            if let Some(key) = environment_session.as_deref() {
                match corpus_core::Project::load(&self.store, &run_id.project) {
                    Ok(project) => {
                        if let Err(error) = corpus_core::close_environment_session_key(
                            &self.store,
                            &project.plugin,
                            key,
                        ) {
                            cleanup_errors.push(format!("environment cleanup failed: {error}"));
                        }
                    }
                    Err(error) => cleanup_errors
                        .push(format!("cannot resolve environment for cleanup: {error}")),
                }
            }
            let error = Error::Store(if cleanup_errors.is_empty() {
                self.finish_run(run_id);
                format!("{rejection}; spawned run was stopped")
            } else {
                let message = format!("{rejection}; cleanup failed: {}", cleanup_errors.join("; "));
                self.fail_run(
                    run_id,
                    RunPhaseKind::Stopping,
                    &Error::Store(message.clone()),
                    true,
                    true,
                );
                message
            });
            return Err(error);
        }
        let notice = ready.notice.take();
        let control_port = ready.session.control_port();
        let child_run_id = ready.session.launch_identity();
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        match ready.mode {
            LaunchMode::DetachedFresh => {
                let tmux = ready
                    .session
                    .pty_attach_command()
                    .and_then(|argv| AppState::pty_attach_session(&argv));
                if let Some(session) = tmux {
                    if let Err(error) = self.bind_fresh_run(
                        &run_id.project,
                        &run_id.mission,
                        Some(session),
                        child_run_id.clone(),
                        control_port,
                    ) {
                        let cleanup = ready.session.stop();
                        let combined = Error::Store(format!(
                            "{error}; spawned run cleanup transcript: {}; cleanup errors: {}",
                            cleanup.transcript.display(),
                            cleanup.cleanup_errors.join("; ")
                        ));
                        self.fail_run(run_id, RunPhaseKind::Running, &combined, false, false);
                        return Err(combined);
                    }
                    self.finish_run(run_id);
                } else {
                    self.background_active_run()?;
                    self.adopt_run(ready.session, run_id.clone());
                    if let Err(error) = self.bind_fresh_run(
                        &run_id.project,
                        &run_id.mission,
                        None,
                        child_run_id.clone(),
                        control_port,
                    ) {
                        return Err(self.cleanup_failed_adoption(run_id, error));
                    }
                }
            }
            LaunchMode::AdoptFresh | LaunchMode::Resume => {
                if let Err(error) = self.background_active_run() {
                    let cleanup = ready.session.stop();
                    let combined = Error::Store(format!(
                        "could not background the current run: {error}; new run cleanup transcript: {}; cleanup errors: {}",
                        cleanup.transcript.display(),
                        cleanup.cleanup_errors.join("; ")
                    ));
                    self.fail_run(run_id, RunPhaseKind::Starting, &combined, true, false);
                    return Err(combined);
                }
                self.adopt_run(ready.session, run_id.clone());
                let binding = match ready.mode {
                    LaunchMode::AdoptFresh => self.bind_fresh_run(
                        &run_id.project,
                        &run_id.mission,
                        self.live_run_session(),
                        child_run_id,
                        control_port,
                    ),
                    LaunchMode::Resume => self.bind_resumed_run(
                        &run_id.project,
                        &run_id.mission,
                        self.live_run_session(),
                        child_run_id,
                        control_port,
                    ),
                    LaunchMode::DetachedFresh => unreachable!(),
                };
                if let Err(error) = binding {
                    return Err(self.cleanup_failed_adoption(run_id, error));
                }
            }
        }
        self.refresh_live_sessions();
        self.refresh_missions(&run_id.project);
        if let Some(mission) = notice {
            self.launch_notices.push(LaunchNotice {
                mission,
                result: Ok(()),
            });
        }
        Ok(())
    }

    /// The shared launch preamble: load the mission, resolve its rev
    /// pins to shas + fetch trees (loud failure here must never tear
    /// down a working run — that happens only after this returns).
    fn prepare_launch(&mut self, run_id: &RunId) -> Result<(Mission, Option<String>), Error> {
        self.run_phases.insert(run_id.clone(), RunPhase::Preparing);
        let cancellation = RunCancellation::default();
        self.run_cancellations
            .insert(run_id.clone(), cancellation.clone());
        let result = (|| {
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch preparation cancelled".into()));
            }
            let mut mission_record =
                load_launchable_mission(&self.store, &run_id.project, &run_id.mission)?;
            let mut effective_pins =
                corpus_observe::project_source_pins(&self.store, &run_id.project)?;
            effective_pins.extend(mission_record.pins.clone());
            mission_record.pins = effective_pins;
            let prepared = self.run_backend.prepare_source_pins(
                &self.store,
                &run_id.project,
                &mission_record.pins,
                &cancellation,
            )?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch preparation cancelled".into()));
            }
            load_launchable_mission(&self.store, &run_id.project, &run_id.mission)?;
            let pins_json = if prepared.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&prepared)?)
            };
            mission_record.environment_session = Some(run_id.storage_key());
            self.store
                .update_mission(&run_id.project, &run_id.mission, &mission_record)?;
            if self
                .environment_runtime
                .open(&self.store, run_id.clone(), prepared)?
                .is_none()
            {
                mission_record.environment_session = None;
                self.store
                    .update_mission(&run_id.project, &run_id.mission, &mission_record)?;
            }
            let final_guard = if cancellation.is_cancelled() {
                Err(Error::Store("launch preparation cancelled".into()))
            } else {
                load_launchable_mission(&self.store, &run_id.project, &run_id.mission).map(drop)
            };
            if let Err(error) = final_guard {
                if let Some(key) = mission_record.environment_session.as_deref() {
                    if let Ok(project) = Project::load(&self.store, &run_id.project) {
                        let _ = corpus_core::close_environment_session_key(
                            &self.store,
                            &project.plugin,
                            key,
                        );
                    }
                }
                return Err(error);
            }
            Ok((mission_record, pins_json))
        })();
        if let Err(error) = &result {
            self.run_cancellations.remove(run_id);
            if cancellation.is_cancelled() {
                self.finish_run(run_id);
            } else {
                self.fail_run(run_id, RunPhaseKind::Preparing, error, true, false);
            }
        }
        result
    }

    /// Request cancellation of the newest preparation for this mission.
    /// The operation owns no child yet; the cancellation gate is checked
    /// before any spawn/adoption can occur.
    pub fn cancel_preparation(&self, project: &str, mission: &str) -> bool {
        if let Some(jobs) = &self.jobs {
            if let Some(run_id) = self
                .run_phases
                .keys()
                .filter(|id| id.project == project && id.mission == mission)
                .max_by_key(|id| id.generation)
            {
                if jobs.cancel_scope(JobKind::LaunchPreparation, run_id) {
                    return true;
                }
            }
        }
        let cancellation = self
            .run_cancellations
            .iter()
            .filter(|(id, _)| id.project == project && id.mission == mission)
            .max_by_key(|(id, _)| id.generation)
            .map(|(_, cancellation)| cancellation);
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
            true
        } else {
            false
        }
    }

    /// Start a new run WITHOUT stopping the live one.
    ///
    /// This used to tear the previous run down. Its guard tested handle
    /// OWNERSHIP rather than reality, so one action behaved two ways: a run
    /// this process launched was killed, while a run it had merely
    /// re-attached to after a restart survived untouched — which is why
    /// restarting the app appeared to "restore" a clobbered session. A run
    /// now ends only when the operator stops it.
    ///
    /// The outgoing run keeps its tmux binding, but `adopt_run` is about to
    /// drop its `RunSession`, and nothing polls for a conversation id once
    /// the handle is gone. So the id is claimed here, while the handle still
    /// exists. `sweep_conversations` is the backstop for a run displaced
    /// before opencode had created its session at all.
    ///
    /// Only a tmux run can be left behind: it is detached, so dropping the
    /// handle costs nothing but the record already names the session. The
    /// piped fallback lives ON the handle — dropping it would orphan the
    /// child and lose the transcript — so that one is stopped (exported)
    /// rather than backgrounded.
    fn background_active_run(&mut self) -> Result<(), Error> {
        if !self.run_active() {
            return Ok(());
        }
        if self.jobs.is_none() {
            self.capture_opencode_session();
        }
        if self.live_pty_attach().is_none() {
            if let Some(attempt) = self.stop_run() {
                if let Some(error) = attempt.error {
                    return Err(error);
                }
            }
        } else {
            // A tmux run is durably detached. Relinquish the app-owned
            // handle and phase before another run is adopted; the mission's
            // recorded session is now its lifecycle truth.
            self.run.take();
            if let Some(run_id) = self.owned_run_id.take() {
                self.finish_run(&run_id);
            }
        }
        Ok(())
    }

    /// Ids already bound to other missions.
    ///
    /// Concurrent runs share one run dir, so `opencode session list` shows
    /// every live mission's conversation. Excluding what is already claimed
    /// is what stops a slow-booting run from adopting a neighbour's.
    fn claimed_conversations(&self, project: &str, except: &str) -> BTreeSet<String> {
        self.trees
            .get(project)
            .into_iter()
            .flat_map(|tree| tree.missions.iter())
            .filter(|(slug, _)| slug.as_str() != except)
            .filter_map(|(_, m)| m.opencode_session.clone())
            .collect()
    }

    /// Fill in the conversation id of any mission that is LIVE in tmux but
    /// has none recorded — the runs no handle covers: displaced by a later
    /// launch, or re-attached after a restart. Without this they stay
    /// orphans, attachable but neither exportable nor resumable.
    pub fn sweep_conversations(&mut self, project: &str) {
        let pending: Vec<(String, String)> = self
            .missions
            .iter()
            .filter(|(_, m)| m.opencode_session.is_none())
            .filter_map(|(slug, m)| Some((slug.clone(), m.session.clone()?)))
            .filter(|(_, session)| self.live_sessions.iter().any(|l| l == session))
            .collect();
        let mut changed = false;
        for (slug, session) in pending {
            let claimed = self.claimed_conversations(project, &slug);
            let Some(launched_at_ms) = launch_stamp_ms(&session) else {
                continue;
            };
            let Ok(id) = self.session_service.find_for_launch(
                &self.store.project_run_dir(project),
                launched_at_ms,
                &claimed,
            ) else {
                continue;
            };
            if self.set_opencode_session(project, &slug, Some(id)).is_ok() {
                changed = true;
            }
        }
        if changed {
            self.refresh_missions(project);
        }
    }

    /// Re-export the opencode transcript of every live mission that has just
    /// finished a turn — i.e. its session settled into `Waiting` (done
    /// working, parked at the prompt). This is what keeps the Cost panel
    /// honest for an ACTIVE conversation: usage updates at each turn
    /// boundary, not only at Stop (which a run killed with the app never
    /// reaches). Keyed by opencode session id, the compact snapshot
    /// overwrites in place without reading message-level data.
    ///
    /// Fires at most once per completed turn: the guard is the session's
    /// last paint (`session_activity`) being newer than our last export
    /// (`last_exported_at`). A session parked quiet since the last export
    /// has nothing new to record and is skipped; a failed export leaves the
    /// stamp untouched, so the next beat simply retries.
    pub(in crate::state) fn sweep_usage_exports(&mut self, project: &str) {
        let pending: Vec<(String, String, String, corpus_core::MissionControl)> = self
            .missions
            .iter()
            .filter_map(|(slug, m)| {
                Some((
                    slug.clone(),
                    m.opencode_session.clone()?,
                    m.session.clone()?,
                    m.control.clone()?,
                ))
            })
            .filter(|(slug, _, _, _)| {
                matches!(
                    self.mission_activity(project, slug),
                    MissionActivity::Waiting
                )
            })
            .filter(|(_, _, tmux, control)| {
                if control.run_id != *tmux {
                    return false;
                }
                // New output since our last export (or never exported) ⇒ a
                // turn happened. No activity reading ⇒ nothing to record.
                should_reexport(
                    self.session_activity.get(tmux).copied(),
                    self.last_exported_at.get(tmux).copied(),
                )
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        let mut changed = false;
        for (_slug, opencode, tmux, control) in pending {
            let session = SessionRef {
                id: opencode,
                directory: self.store.project_run_dir(project),
            };
            let captured = corpus_core::opencode_control_password(&self.store, &control.run_id)
                .map_err(|error| error.to_string())
                .and_then(|password| {
                    self.session_service
                        .usage_snapshot(&control, &password, &session)
                })
                .and_then(|snapshot| {
                    self.store
                        .write_usage_snapshot(project, &snapshot)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                });
            if captured.is_ok() {
                self.last_exported_at
                    .insert(tmux, self.clock.monotonic_now());
                changed = true;
            }
        }
        if changed {
            // Fold the fresh exports into the Cost panel straight away.
            self.note_corpus_mutation(project);
            self.refresh_corpus_stats(project);
        }
    }

    /// Point a mission at a tmux session (or clear a dead one). The
    /// opencode session is left alone: the tmux session is where the run
    /// is ATTACHED, while the opencode session is what the mission IS —
    /// it outlives every attach and is what `resume_mission` re-opens.
    pub(in crate::state) fn set_tmux_session(
        &mut self,
        project: &str,
        slug: &str,
        session: Option<String>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.session = session;
        mission.control = None;
        self.store.update_mission(project, slug, &mission)
    }

    pub(in crate::state) fn bind_resumed_run(
        &mut self,
        project: &str,
        slug: &str,
        session: Option<String>,
        run_id: Option<String>,
        control_port: Option<u16>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.session = session;
        mission.control = run_id
            .zip(control_port)
            .map(|(run_id, port)| corpus_core::MissionControl { run_id, port });
        self.store.update_mission(project, slug, &mission)
    }

    /// Bind a newly spawned run to its mission in one read-modify-write.
    /// A fresh run always starts a fresh opencode conversation, so the tmux
    /// session and old conversation id must never be committed separately.
    pub(in crate::state) fn bind_fresh_run(
        &mut self,
        project: &str,
        slug: &str,
        session: Option<String>,
        child_run_id: Option<String>,
        control_port: Option<u16>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        let piped_child = session.is_none() && child_run_id.is_some();
        mission.session = session;
        mission.control = child_run_id
            .clone()
            .zip(control_port)
            .map(|(run_id, port)| corpus_core::MissionControl { run_id, port });
        mission.opencode_session = None;
        if let Some(dispatch) = mission
            .dispatch
            .as_mut()
            .filter(|dispatch| dispatch.completion.is_none() && dispatch.child_run_id.is_none())
        {
            dispatch.child_run_id = child_run_id;
            dispatch.live_seen = piped_child;
            dispatch.running_seen = piped_child;
        }
        self.store.update_mission(project, slug, &mission)
    }

    /// A process exists but its durable mission binding failed. The app
    /// already owns cleanup responsibility, so stop it before returning and
    /// preserve both the primary failure and cleanup evidence in the phase.
    fn cleanup_failed_adoption(&mut self, run_id: &RunId, primary: Error) -> Error {
        let cleanup = self.stop_run();
        let cleanup_pending = cleanup
            .as_ref()
            .is_some_and(|attempt| !attempt.cleanup_complete);
        let detail = match &cleanup {
            Some(attempt) => match &attempt.error {
                Some(error) => format!(
                    "{primary}; spawned run cleanup also failed: {error}; transcript: {}",
                    attempt.transcript.display()
                ),
                None => format!(
                    "{primary}; spawned run was stopped; transcript: {}",
                    attempt.transcript.display()
                ),
            },
            None => format!("{primary}; spawned run cleanup produced no transcript"),
        };
        let combined = Error::Store(detail);
        self.fail_run(
            run_id,
            RunPhaseKind::Running,
            &combined,
            cleanup_pending,
            cleanup_pending,
        );
        combined
    }

    /// Record (or clear) the opencode conversation a mission owns. A
    /// fresh launch clears it — opencode starts a new conversation, so
    /// the old id would resume the WRONG one — and discovery fills it
    /// back in once the TUI has created its session.
    pub(in crate::state) fn set_opencode_session(
        &mut self,
        project: &str,
        slug: &str,
        id: Option<String>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.opencode_session = id;
        self.store.update_mission(project, slug, &mission)
    }

    /// Catch the opencode session id of the live run and persist it on
    /// its mission. Called on the poll beat: the id doesn't exist at
    /// launch (the TUI has to boot first), and without it a mission can
    /// be neither exported after an app restart nor resumed. Cheap once
    /// it lands — the lookup stops the moment the record has an id.
    pub(in crate::state) fn capture_opencode_session(&mut self) {
        let Some(run_id) = self.owned_run_id.clone() else {
            return;
        };
        if !self.run_active() || self.run_phase(&run_id) != RunPhase::Running {
            return;
        }
        let known = self
            .store
            .load_mission(&run_id.project, &run_id.mission)
            .ok()
            .and_then(|m| m.opencode_session);
        if known.is_some() {
            return;
        }
        let claimed = self.claimed_conversations(&run_id.project, &run_id.mission);
        let Some(id) = self
            .run
            .as_mut()
            .and_then(|run| run.opencode_session_id(&claimed))
        else {
            return;
        };
        self.apply_discovered_conversation(&run_id, id);
    }

    /// Apply discovery only to the exact generation that requested it.
    /// Late background results from an older launch are harmless values.
    pub(in crate::state) fn apply_discovered_conversation(
        &mut self,
        run_id: &RunId,
        id: String,
    ) -> bool {
        if self.owned_run_id.as_ref() != Some(run_id) || self.run_phase(run_id) != RunPhase::Running
        {
            return false;
        }
        if self
            .set_opencode_session(&run_id.project, &run_id.mission, Some(id))
            .is_err()
        {
            return false;
        }
        self.refresh_missions(&run_id.project);
        true
    }

    /// Rename a mission (its display label) while keeping the slug.
    pub fn rename_mission(&mut self, project: &str, slug: &str, name: &str) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        let name = name.trim();
        mission.name = if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        };
        self.store.update_mission(project, slug, &mission)
    }

    /// Stop a mission's run: attempt transcript export and cleanup, surfacing
    /// each failure — whether the app owns the run or it survived an app
    /// relaunch. Clears the dead tmux session and returns the durable
    /// transcript path when known. The opencode session id STAYS on the
    /// record: stopping ends the attach, not the conversation, and that
    /// id is what `resume_mission` re-opens.
    pub fn stop_mission(&mut self, project: &str, slug: &str) -> Result<StopMissionResult, Error> {
        let mission = self.store.load_mission(project, slug)?;
        let session = mission.session.as_deref();
        if session.is_none() && !self.mission_environment_needs_cleanup(project, slug) {
            return Err(Error::Store(
                "no live session or environment on this mission — nothing to stop".into(),
            ));
        }
        if session.is_none() {
            let project_record = corpus_core::Project::load(&self.store, project)?;
            let key = mission
                .environment_session
                .as_deref()
                .ok_or_else(|| Error::Store("environment cleanup identity disappeared".into()))?;
            if matches!(
                self.store
                    .load_environment_session_key(&project_record.plugin, key),
                Err(corpus_core::Error::Io(ref error))
                    if error.kind() == std::io::ErrorKind::NotFound
            ) {
                // The mission key is written immediately before the durable
                // Opening record. If the app died in that tiny interval, no
                // plugin mutation was attempted and clearing the unmatched
                // key is the complete recovery action.
                let mut repaired = mission.clone();
                repaired.environment_session = None;
                self.store.update_mission(project, slug, &repaired)?;
                return Ok(StopMissionResult::Completed(String::new()));
            }
        }
        if self.jobs.is_some() {
            self.schedule_teardown(project, slug, &mission, session)?;
            return Ok(StopMissionResult::Scheduled);
        }
        if session.is_none() {
            let run_id = self.next_run_id(project, slug);
            self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
            let project_record = corpus_core::Project::load(&self.store, project)?;
            let environment_session = mission
                .environment_session
                .as_deref()
                .ok_or_else(|| Error::Store("environment cleanup identity disappeared".into()))?;
            match corpus_core::close_environment_session_key(
                &self.store,
                &project_record.plugin,
                environment_session,
            ) {
                Ok(()) => {
                    self.finish_run(&run_id);
                    return Ok(StopMissionResult::Completed(String::new()));
                }
                Err(error) => {
                    self.fail_run(&run_id, RunPhaseKind::Stopping, &error, true, true);
                    return Err(error);
                }
            }
        }
        let session = session.expect("checked above");
        let (path, mut error, mut cleanup_complete) = if self.live_run_session().as_deref()
            == Some(session)
        {
            let attempt = self.stop_run().ok_or_else(|| {
                Error::Store("run ownership disappeared before Stop could begin".into())
            })?;
            (
                Some(attempt.transcript.display().to_string()),
                attempt.error,
                attempt.cleanup_complete,
            )
        } else {
            let run_id = self.next_run_id(project, slug);
            self.run_phases.insert(run_id.clone(), RunPhase::Exporting);
            // A run that outlived the app: stop the writer before exporting
            // so OpenCode cannot return a partial JSON string. Both outcomes
            // are still reported independently.
            self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
            let cleanup_error = self
                .run_backend
                .kill_tmux_session(session)
                .err()
                .map(|error| format!("session cleanup failed: {error}"));
            let (exported, export_error) = match mission.opencode_session.as_deref() {
                Some(id) => match self.run_backend.export_session(project, id) {
                    Ok(path) => (Some(path.display().to_string()), None),
                    Err(error) => (None, Some(format!("transcript export failed: {error}"))),
                },
                None => (None, None),
            };
            let cleanup_complete = cleanup_error.is_none();
            let error = match (export_error, cleanup_error) {
                (None, None) => None,
                (Some(export), None) => Some(Error::Store(export)),
                (None, Some(cleanup)) => Some(Error::Store(cleanup)),
                (Some(export), Some(cleanup)) => Some(Error::Store(format!("{export}; {cleanup}"))),
            };
            if cleanup_complete {
                self.finish_run(&run_id);
                if let Some(error) = &error {
                    self.fail_run(&run_id, RunPhaseKind::Exporting, error, true, false);
                }
            } else {
                self.fail_run(
                    &run_id,
                    RunPhaseKind::Stopping,
                    error.as_ref().unwrap(),
                    true,
                    true,
                );
            }
            (exported, error, cleanup_complete)
        };
        if let Some(environment_session) = mission.environment_session.as_deref() {
            if let Ok(project_record) = corpus_core::Project::load(&self.store, project) {
                if let Err(environment_error) = corpus_core::close_environment_session_key(
                    &self.store,
                    &project_record.plugin,
                    environment_session,
                ) {
                    cleanup_complete = false;
                    error = Some(Error::Store(match error {
                        Some(existing) => {
                            format!("{existing}; environment cleanup failed: {environment_error}")
                        }
                        None => format!("environment cleanup failed: {environment_error}"),
                    }));
                }
            }
        }
        // Clear the durable binding only when cleanup actually completed.
        // A failed kill keeps the identity available for a retry.
        if cleanup_complete {
            self.set_tmux_session(project, slug, None)?;
        }
        self.refresh_live_sessions();
        if let Some(error) = error {
            return Err(error);
        }
        Ok(StopMissionResult::Completed(path.unwrap_or_default()))
    }

    fn schedule_teardown(
        &mut self,
        project: &str,
        slug: &str,
        mission: &Mission,
        tmux: Option<&str>,
    ) -> Result<(), Error> {
        let owned = tmux.is_some() && self.live_run_session().as_deref() == tmux;
        let run_id = if owned {
            self.owned_run_id.take().ok_or_else(|| {
                Error::Store("run ownership disappeared before Stop could begin".into())
            })?
        } else {
            self.next_run_id(project, slug)
        };
        self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
        let retained = if owned { self.run.take() } else { None };
        let backend = self.run_backend.clone();
        let store = self.store.clone();
        let project_owned = project.to_string();
        let conversation = mission.opencode_session.clone();
        let environment_session = mission.environment_session.clone();
        let plugin_id = corpus_core::Project::load(&self.store, project)
            .ok()
            .map(|project| project.plugin);
        let tmux_owned = tmux.map(str::to_string);
        let session_operation = self.session_operation_leases.claim(project, slug);
        let scope = self.job_scope(project, Some(run_id));
        self.jobs.as_mut().expect("installed above").start(
            JobKind::SessionTeardown,
            scope,
            Duration::from_secs(30),
            move |_| {
                let _ownership = session_operation
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(mut session) = retained {
                    let outcome = session.stop();
                    let mut errors = outcome.cleanup_errors.clone();
                    if let Some(error) = outcome.export_error {
                        errors.insert(0, error);
                    }
                    let mut environment_cleanup_failed = false;
                    if let (Some(plugin_id), Some(environment_session)) =
                        (plugin_id.as_deref(), environment_session.as_deref())
                    {
                        if let Err(error) = corpus_core::close_environment_session_key(
                            &store,
                            plugin_id,
                            environment_session,
                        ) {
                            environment_cleanup_failed = true;
                            errors.push(format!("environment cleanup failed: {error}"));
                        }
                    }
                    let cleanup_complete =
                        outcome.cleanup_errors.is_empty() && !environment_cleanup_failed;
                    return Ok(AppJobOutput::TeardownReady(TeardownReady {
                        transcript: Some(outcome.transcript.display().to_string()),
                        error: (!errors.is_empty()).then(|| errors.join("; ")),
                        cleanup_complete,
                        retained: (!cleanup_complete).then_some(session),
                    }));
                }
                // Stabilize OpenCode's session data before asking the CLI to
                // serialize it. A live streaming response can otherwise end
                // in a successful but truncated export.
                let cleanup_error = tmux_owned.as_deref().and_then(|tmux| {
                    backend
                        .kill_tmux_session(tmux)
                        .err()
                        .map(|error| format!("session cleanup failed: {error}"))
                });
                let (transcript, export_error) = match conversation.as_deref() {
                    Some(conversation) => {
                        match backend.export_session(&project_owned, conversation) {
                            Ok(path) => (Some(path.display().to_string()), None),
                            Err(error) => {
                                (None, Some(format!("transcript export failed: {error}")))
                            }
                        }
                    }
                    None => (None, None),
                };
                let environment_error = match (plugin_id.as_deref(), environment_session.as_deref())
                {
                    (Some(plugin_id), Some(environment_session)) => {
                        corpus_core::close_environment_session_key(
                            &store,
                            plugin_id,
                            environment_session,
                        )
                        .err()
                        .map(|error| format!("environment cleanup failed: {error}"))
                    }
                    _ => None,
                };
                let cleanup_complete = cleanup_error.is_none() && environment_error.is_none();
                let error = [export_error, cleanup_error, environment_error]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                Ok(AppJobOutput::TeardownReady(TeardownReady {
                    transcript,
                    error: (!error.is_empty()).then(|| error.join("; ")),
                    cleanup_complete,
                    retained: None,
                }))
            },
        );
        Ok(())
    }

    pub(in crate::state) fn apply_teardown_ready(
        &mut self,
        run_id: &RunId,
        ready: TeardownReady,
    ) -> (bool, String) {
        let delete_key = (run_id.project.clone(), run_id.mission.clone());
        let delete_requested = self.pending_mission_deletes.contains(&delete_key);
        if ready.cleanup_complete {
            if let Err(error) = self.set_tmux_session(&run_id.project, &run_id.mission, None) {
                self.fail_run(run_id, RunPhaseKind::Stopping, &error, true, false);
                return (true, error.to_string());
            }
            self.finish_run(run_id);
            self.run_meta = None;
            if delete_requested {
                if let Err(error) = self.store.delete_mission(&run_id.project, &run_id.mission) {
                    self.pending_mission_deletes.remove(&delete_key);
                    return (true, error.to_string());
                }
                self.pending_mission_deletes.remove(&delete_key);
                if self.selected_mission.as_deref() == Some(run_id.mission.as_str()) {
                    self.selected_mission = None;
                }
            }
        } else if let Some(session) = ready.retained {
            self.run = Some(session);
            self.owned_run_id = Some(run_id.clone());
            let error = Error::Store(
                ready
                    .error
                    .clone()
                    .unwrap_or_else(|| "session cleanup failed".into()),
            );
            self.fail_run(run_id, RunPhaseKind::Stopping, &error, true, true);
        }
        self.refresh_live_sessions();
        self.refresh_missions(&run_id.project);
        if let Some(error) = ready.error {
            if delete_requested && ready.cleanup_complete {
                (true, format!("mission deleted; {error}"))
            } else {
                (true, error)
            }
        } else {
            // Export still happens; the path belongs in the run store, not a
            // routine success notification.
            drop(ready.transcript);
            (
                false,
                if delete_requested {
                    "mission deleted"
                } else {
                    "mission stopped"
                }
                .into(),
            )
        }
    }
}
