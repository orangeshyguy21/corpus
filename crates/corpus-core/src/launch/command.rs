//! Plan-derived child environment and OpenCode command construction.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use super::plan::LaunchPlan;
use super::policy::opencode_agent_handle;
use crate::error::Result;
use crate::store::{
    Store, AGENT_ENV, ENVIRONMENT_SESSION_ENV, HANDLE_ENV, MISSION_ENV, PROJECT_ENV, RUN_ID_ENV,
    RUN_LOG_ENV, SOURCE_PINS_ENV, STORE_ENV,
};

/// Owned environment projection shared by TUI scripts and piped children.
/// Dynamic launch identity is derived once from the immutable plan and the
/// backend's already-claimed run/log identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LaunchEnvironment {
    entries: Vec<(String, String)>,
}

/// Identifiers claimed by a concrete backend after the shared plan is fixed.
pub(super) struct BackendIdentity<'a> {
    pub(super) opencode: &'a Path,
    pub(super) agent_stem: &'a str,
    pub(super) handle: &'a str,
    pub(super) run_log: Option<&'a str>,
    pub(super) run_identity: Option<&'a str>,
    pub(super) control_password: Option<&'a str>,
}

impl LaunchEnvironment {
    pub(super) fn from_plan(
        store: &Store,
        plan: &LaunchPlan,
        backend: BackendIdentity<'_>,
    ) -> Self {
        let mut entries = vec![
            (
                "CORPUS_OPENCODE_BIN".into(),
                backend.opencode.to_string_lossy().into_owned(),
            ),
            (AGENT_ENV.into(), backend.agent_stem.into()),
            (HANDLE_ENV.into(), backend.handle.into()),
            (PROJECT_ENV.into(), plan.project().into()),
            (
                STORE_ENV.into(),
                store.root().to_string_lossy().into_owned(),
            ),
        ];
        if let Some(model) = plan.model() {
            entries.push(("CORPUS_OPENCODE_MODEL".into(), model.into()));
        }
        if let Some(pins) = plan.source_pins_json() {
            entries.push((SOURCE_PINS_ENV.into(), pins.into()));
        }
        if let Some(session) = plan.environment_session() {
            entries.push((ENVIRONMENT_SESSION_ENV.into(), session.into()));
        }
        if let Some(log) = backend.run_log {
            entries.push((RUN_LOG_ENV.into(), log.into()));
        }
        if let Some(run_id) = plan.run_id() {
            entries.push((MISSION_ENV.into(), run_id.mission.clone()));
            if let Some(identity) = backend.run_identity {
                entries.push((RUN_ID_ENV.into(), identity.into()));
            }
        }
        if let Some(password) = backend.control_password {
            entries.push(("OPENCODE_SERVER_PASSWORD".into(), password.into()));
        }
        Self { entries }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// Build the piped `opencode run` child without spawning it.
pub(super) fn opencode_command(
    opencode: &Path,
    store: &Store,
    plan: &LaunchPlan,
    environment: &LaunchEnvironment,
) -> Result<Command> {
    let mut command = Command::new(opencode);
    let handle = opencode_agent_handle(store, plan.project(), plan.agent());
    command.args(["run", "--agent", &handle]);
    if let Some(model) = plan.model() {
        command.args(["-m", model]);
    }
    if !plan.mission().trim().is_empty() {
        command.arg(plan.mission());
    }
    command.current_dir(
        store.provision_run_dir_with_sources(plan.project(), plan.source_pins_json())?,
    );
    for (key, value) in environment.iter() {
        command.env(key, value);
    }
    Ok(command)
}

/// Write the owner-only script executed by the detached tmux pane.
pub(super) fn write_tui_script(
    script: &Path,
    params: &[(&str, &str)],
    prompt: Option<&str>,
    resume: Option<&str>,
    control_port: Option<u16>,
) -> Result<()> {
    let mut out = String::from("#!/bin/sh\n");
    for (key, value) in params {
        out.push_str(&format!("export {key}={}\n", shell_quote(value)));
    }
    let mut exec = make_exec_vars();
    if let Some(port) = control_port {
        exec.push_str(&format!(" --hostname 127.0.0.1 --port {port}"));
    }
    if let Some(id) = resume {
        exec.push_str(&format!(" --session {}", shell_quote(id)));
    }
    if let Some(prompt) = prompt {
        exec.push_str(&format!(" --prompt {}", shell_quote(prompt)));
    }
    out.push_str(&format!("exec {exec}\n"));
    fs::write(script, out)?;
    let mut permissions = fs::metadata(script)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(script, permissions)?;
    Ok(())
}

fn make_exec_vars() -> String {
    "\"$CORPUS_OPENCODE_BIN\" --agent \"$CORPUS_OPENCODE_HANDLE\" --model \"$CORPUS_OPENCODE_MODEL\"".into()
}

/// Single-quote a dynamic value so it is inert inside the run script.
pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::plan::LaunchPlan;

    #[test]
    fn environment_projection_carries_exact_plan_identity() {
        let store = Store::new("/tmp/corpus-command-test/store".into());
        let run_id = corpus_store::EnvironmentSessionId {
            project: "alpha".into(),
            mission: "curator".into(),
            generation: 2,
        };
        let plan = LaunchPlan::interactive(
            "alpha",
            "runner",
            "mlx/qwen3.8".into(),
            "probe",
            Some("{\"repo\":\"sha\"}"),
            Some("environment-key"),
            Some(&run_id),
        );
        let environment = LaunchEnvironment::from_plan(
            &store,
            &plan,
            BackendIdentity {
                opencode: Path::new("/bin/opencode"),
                agent_stem: "runner",
                handle: "Runner",
                run_log: Some("run.raw"),
                run_identity: Some("corpus-run-1"),
                control_password: Some("secret"),
            },
        );
        let entries: std::collections::BTreeMap<_, _> = environment.iter().collect();
        assert_eq!(entries[AGENT_ENV], "runner");
        assert_eq!(entries[HANDLE_ENV], "Runner");
        assert_eq!(entries[MISSION_ENV], "curator");
        assert_eq!(entries[RUN_ID_ENV], "corpus-run-1");
        assert_eq!(entries[RUN_LOG_ENV], "run.raw");
        assert_eq!(entries[SOURCE_PINS_ENV], "{\"repo\":\"sha\"}");
        assert_eq!(entries[ENVIRONMENT_SESSION_ENV], "environment-key");
    }

    #[test]
    fn shell_rendering_quotes_every_dynamic_value() {
        let quoted = shell_quote("x'; rm -rf /tmp/nope; #");
        assert_eq!(quoted, "'x'\\''; rm -rf /tmp/nope; #'");
    }
}
