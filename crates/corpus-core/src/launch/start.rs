//! Construction and compatibility routing for launched sessions.

use std::fs;
use std::path::{Path, PathBuf};

use super::command::{opencode_command, write_tui_script, BackendIdentity, LaunchEnvironment};
use super::executables::{resolve_opencode, tmux_available};
use super::plan::{LaunchMode, LaunchPlan};
use super::policy::{
    allocate_control_port, opencode_agent_handle, opencode_control_password, resolve_launch_model,
};
use super::process::spawn_piped;
use super::session::{Backend, PipedBackend, RunSession, TuiBackend};
use super::tmux::{start_session, SessionSetup};
use super::transcript::{artifact_stem, create_piped};
use crate::error::{Error, Result};
use crate::store::Store;
use corpus_store::EnvironmentSessionId;

impl RunSession {
    /// APP launch: resolve the model (primary-model -> arg -> registry
    /// tool-use default, fail loudly if none), then run the FULL TUI in
    /// a detached tmux session, or the piped headless fallback when tmux
    /// is absent. `source_pins_json` is the RESOLVED `repo -> sha` map
    /// (from `registry::prepare_source_pins`, trees already fetched) —
    /// exported as CORPUS_SOURCE_PINS so the sandbox mounts exactly the
    /// revs the mission recorded; None = the plugin's default pins.
    pub fn spawn(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
    ) -> Result<Self> {
        Self::spawn_with_environment(project, agent, model, mission, source_pins_json, None)
    }

    pub fn spawn_with_environment(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<Self> {
        Self::spawn_with_identity(
            project,
            agent,
            model,
            mission,
            source_pins_json,
            environment_session,
            None,
        )
    }

    /// App mission launch with a generation-specific identity exported to the
    /// agent process. Headless/manual callers use `spawn_with_environment` and
    /// intentionally receive no automatic Curator return address.
    pub fn spawn_mission_with_environment(
        run_id: &EnvironmentSessionId,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<Self> {
        Self::spawn_with_identity(
            &run_id.project,
            agent,
            model,
            mission,
            source_pins_json,
            environment_session,
            Some(run_id),
        )
    }

    fn spawn_with_identity(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        run_id: Option<&EnvironmentSessionId>,
    ) -> Result<Self> {
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        let model = resolve_launch_model(&store, project, agent, model)?;
        let plan = LaunchPlan::interactive(
            project,
            agent,
            model,
            mission,
            source_pins_json,
            environment_session,
            run_id,
        );
        Self::execute_plan(&store, &plan)
    }

    /// Re-open an EXISTING opencode session in a fresh TUI
    /// (`opencode --session <id>`): the conversation comes back with its
    /// history, so a mission whose tmux session is long dead is steerable
    /// again instead of being a dead record.
    ///
    /// The session id is known up front, so it seeds `tui_session_id` —
    /// a resumed run can export from its first moment (the launch-window
    /// search that a fresh spawn needs would never match an old session).
    /// TUI only: resuming is an interactive act, and the piped backend
    /// has no session to return to.
    pub fn resume(
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
    ) -> Result<Self> {
        Self::resume_with_environment(
            project,
            agent,
            model,
            opencode_session_id,
            source_pins_json,
            None,
        )
    }

    pub fn resume_with_environment(
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<Self> {
        Self::resume_with_identity(
            project,
            agent,
            model,
            opencode_session_id,
            source_pins_json,
            environment_session,
            None,
        )
    }

    pub fn resume_mission_with_environment(
        run_id: &EnvironmentSessionId,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<Self> {
        Self::resume_with_identity(
            &run_id.project,
            agent,
            model,
            opencode_session_id,
            source_pins_json,
            environment_session,
            Some(run_id),
        )
    }

    fn resume_with_identity(
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        run_id: Option<&EnvironmentSessionId>,
    ) -> Result<Self> {
        if tmux_available().is_none() {
            return Err(Error::Store(
                "resume needs tmux — the piped backend has no session to re-open".into(),
            ));
        }
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        let model = resolve_launch_model(&store, project, agent, model)?;
        let plan = LaunchPlan::resume(
            project,
            agent,
            model,
            opencode_session_id,
            source_pins_json,
            environment_session,
            run_id,
        );
        Self::execute_plan(&store, &plan)
    }

    /// CLI automation: always the headless `opencode run` piped path.
    /// No model resolution — `-m` stays optional (scripted missions may
    /// lean on opencode's own default-resolver).
    pub fn spawn_headless(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
    ) -> Result<Self> {
        Self::spawn_headless_with_environment(project, agent, model, mission, None, None)
    }

    pub fn spawn_headless_with_environment(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<Self> {
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        let plan = LaunchPlan::headless(
            project,
            agent,
            model,
            mission,
            None,
            source_pins,
            environment_session,
        );
        Self::execute_plan(&store, &plan)
    }

    /// CLI automation APPENDING to an existing transcript (the
    /// researcher follow-up pass).
    pub fn spawn_headless_append(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        append_to: &Path,
    ) -> Result<Self> {
        Self::spawn_headless_append_with_environment(
            project, agent, model, mission, append_to, None, None,
        )
    }

    pub fn spawn_headless_append_with_environment(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        append_to: &Path,
        source_pins: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<Self> {
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        let plan = LaunchPlan::headless(
            project,
            agent,
            model,
            mission,
            Some(append_to),
            source_pins,
            environment_session,
        );
        Self::execute_plan(&store, &plan)
    }

    /// Route one immutable plan to its backend. All compatibility entry points
    /// converge here before command construction starts.
    fn execute_plan(store: &Store, plan: &LaunchPlan) -> Result<Self> {
        match plan.mode() {
            LaunchMode::Interactive if tmux_available().is_some() => Self::start_tui(store, plan),
            LaunchMode::Interactive => Self::start_piped(store, plan),
            LaunchMode::Resume { .. } => {
                if tmux_available().is_none() {
                    return Err(Error::Store(
                        "resume needs tmux — the piped backend has no session to re-open".into(),
                    ));
                }
                Self::start_tui(store, plan)
            }
            LaunchMode::Headless { .. } => Self::start_piped(store, plan),
        }
    }

    /// The full opencode TUI in a detached tmux session. `resume` carries
    /// an existing opencode session id to re-open (`--session`) instead of
    /// starting a fresh conversation.
    fn start_tui(store: &Store, plan: &LaunchPlan) -> Result<Self> {
        let project = plan.project();
        let agent = plan.agent();
        let mission = plan.mission();
        let run_id = plan.run_id();
        let resume = plan.resume_session();
        if plan.model().is_none() {
            return Err(Error::Store("interactive launch plan has no model".into()));
        }
        let opencode = resolve_opencode()?;
        let ts = now_secs();
        // Two identifiers, on purpose: `agent_stem` is the dir slug (the
        // run's identity, resolved to a role server-side); `handle` is the
        // name opencode shows and loads `--agent` by. They coincide for an
        // unnamed agent.
        let agent_stem = crate::store::slugify(agent);
        let artifact_stem = artifact_stem(&agent_stem, run_id);
        let handle = opencode_agent_handle(store, project, agent);
        // The session and raw capture share one run stem. App launches add
        // mission generation to prevent same-agent, same-second collisions;
        // manual launches retain the historical agent-only stem.
        let session = format!("corpus-{artifact_stem}-{ts}");
        let export_json = Self::runs_for(store, project, &artifact_stem, ts, "json");
        let temp = std::env::temp_dir();
        // The raw capture is a CORPUS ARTIFACT, not a temp file: pipe-pane
        // appends to it from the first output, so the run leaves a durable
        // log in the project corpus runs/ even if the app dies, the export
        // never happens, or the session is never stopped.
        let raw = Self::runs_for(store, project, &artifact_stem, ts, "raw");
        let script = temp.join(format!("{session}.sh"));

        let control_port = run_id.map(|_| allocate_control_port()).transpose()?;
        let control_password = control_port
            .map(|_| opencode_control_password(store, &session))
            .transpose()?;

        let workspace =
            store.provision_run_workspace_with_sources(project, plan.source_pins_json())?;
        let repo = workspace.path;
        let prompt = if mission.trim().is_empty() {
            None
        } else {
            Some(mission)
        };
        let raw_log = raw
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let environment = LaunchEnvironment::from_plan(
            store,
            plan,
            BackendIdentity {
                opencode: &opencode,
                agent_stem: &agent_stem,
                handle: &handle,
                run_log: Some(&raw_log),
                run_identity: Some(&session),
                control_password: control_password.as_deref(),
            },
        );
        let script_environment: Vec<_> = environment.iter().collect();
        write_tui_script(&script, &script_environment, prompt, resume, control_port)?;
        // Stamped BEFORE the spawn: session discovery keys off "created
        // after this moment", and a stamp taken afterwards could in
        // principle sit past the session opencode went on to create.
        let launched_at_ms = now_millis();
        if let Err(error) = start_session(SessionSetup {
            name: &session,
            cwd: &repo,
            script: &script,
            raw_capture: &raw,
            environment: &environment,
        }) {
            let _ = fs::remove_file(&script);
            return Err(error);
        }
        Ok(Self {
            transcript: export_json.clone(),
            backend: Backend::Tui(Box::new(TuiBackend {
                session,
                workspace_id: workspace.id,
                control_port,
                // A resume already knows its session; a fresh spawn
                // discovers one once opencode has created it.
                tui_session_id: resume.map(str::to_string),
                launched_at_ms,
                stopped: false,
                exported: false,
                export_json,
                raw,
                script,
                file_pos: 0,
                pending: String::new(),
                liveness: (std::time::Instant::now(), true),
                discovery: std::time::Instant::now(),
                repo,
            })),
        })
    }

    /// The piped headless backend: `opencode run` with streams pumped
    /// into the transcript and a line channel.
    fn start_piped(store: &Store, plan: &LaunchPlan) -> Result<Self> {
        let project = plan.project();
        let agent = plan.agent();
        let model = plan.model();
        let mission = plan.mission();
        let append_to = plan.append_to();
        let run_id = plan.run_id();
        let opencode = resolve_opencode()?;
        let runs = store.project_corpus_dir(project).join("runs");
        let transcript = match append_to {
            Some(path) => path.to_path_buf(),
            None => {
                let artifact_stem = artifact_stem(agent, run_id);
                create_piped(&runs, &artifact_stem, agent, model, mission)?
            }
        };
        let agent_stem = crate::store::slugify(agent);
        let handle = opencode_agent_handle(store, project, agent);
        let transcript_name = transcript
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let environment = LaunchEnvironment::from_plan(
            store,
            plan,
            BackendIdentity {
                opencode: &opencode,
                agent_stem: &agent_stem,
                handle: &handle,
                run_log: transcript_name.as_deref(),
                run_identity: transcript_name.as_deref(),
                control_password: None,
            },
        );
        let command = opencode_command(&opencode, store, plan, &environment)?;
        let (child, rx) = spawn_piped(command, &transcript)?;
        Ok(Self {
            transcript,
            backend: Backend::Piped(PipedBackend { child, rx }),
        })
    }

    fn runs_for(store: &Store, project: &str, agent: &str, ts: u64, ext: &str) -> PathBuf {
        store
            .project_corpus_dir(project)
            .join("runs")
            .join(format!("{ts}-{agent}.{ext}"))
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
