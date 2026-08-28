use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::support::{core_project, tmp_store};
use crate::launch::command::{
    opencode_command, write_tui_script, BackendIdentity, LaunchEnvironment,
};
use crate::launch::executables::tmux_available;
use crate::launch::plan::LaunchPlan;
use crate::launch::session::{Backend, RunSession};
use crate::store::{AGENT_ENV, ENVIRONMENT_SESSION_ENV};
use crate::test_support::{env_lock, unique_temp_path, EnvVarGuard};
use corpus_store::EnvironmentSessionId;

/// Integration test (env-locked): the spawn/stop machinery
/// runs against a temp store with agents created explicitly by role,
/// exercising the teamless paths.
#[test]
#[ignore = "platform: sends signals to an owned Unix process group"]
fn spawn_stop_and_piped_headless() {
    let _guard = env_lock();
    let bin = unique_temp_path("corpus-fake-bin");
    let child_pid_file = unique_temp_path("corpus-fake-child-pid");
    let _ = fs::remove_dir_all(&bin);
    fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("opencode");
    fs::write(
        &fake,
        "#!/bin/sh\nsleep 90127 &\nchild=$!\nprintf '%s\\n' \"$child\" > \"$CORPUS_TEST_CHILD_PID\"\nwait \"$child\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::env::var("PATH").unwrap_or_default();
    path = format!("{}:{}", bin.display(), path);
    let _path = EnvVarGuard::set("PATH", &path);
    let _child_pid = EnvVarGuard::set("CORPUS_TEST_CHILD_PID", &child_pid_file);

    let (store, store_dir) = tmp_store("stop-v2");
    let _store = EnvVarGuard::set("CORPUS_STORE", &store_dir);
    core_project(&store);

    let mut session = RunSession::spawn_headless("default", "operator", None, "probe")
        .expect("piped headless spawn");
    assert!(session.transcript.is_file(), "transcript starts at spawn");
    std::thread::sleep(Duration::from_millis(800));
    let child_pid = fs::read_to_string(&child_pid_file)
        .expect("fake child publishes its pid")
        .trim()
        .to_string();

    let started = std::time::Instant::now();
    let stopped_at = session.stop();
    assert_eq!(
        stopped_at, session.transcript,
        "stop returns the transcript"
    );
    let mut exited = false;
    while started.elapsed() < Duration::from_secs(5) {
        if session.try_exit().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited, "stop reaps the run within 5s");
    let alive = Command::new("kill")
        .args(["-0", &child_pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    assert!(!alive, "no orphaned grandchildren");

    // Transcript is in the project corpus runs/.
    let runs_dir = store.project_corpus_dir("default").join("runs");
    assert!(
        runs_dir
            .join(session.transcript.file_name().unwrap())
            .exists(),
        "transcript in project corpus"
    );

    let _ = fs::remove_dir_all(&bin);
    let _ = fs::remove_file(&child_pid_file);
    let _ = fs::remove_dir_all(&store_dir);
}

#[test]
fn app_mission_launch_exports_exact_mission_and_run_identity() {
    let _guard = env_lock();
    let bin = unique_temp_path("corpus-fake-origin-bin");
    let _ = fs::remove_dir_all(&bin);
    fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("opencode");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s|%s\\n' \"$CORPUS_MISSION\" \"$CORPUS_RUN_ID\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::env::var("PATH").unwrap_or_default();
    path = format!("{}:{}", bin.display(), path);
    let _path = EnvVarGuard::set("PATH", &path);
    let _no_tmux = EnvVarGuard::set("CORPUS_NO_TMUX", "1");

    let (store, store_dir) = tmp_store("mission-origin");
    let _store = EnvVarGuard::set("CORPUS_STORE", &store_dir);
    core_project(&store);
    let run_id = EnvironmentSessionId {
        project: "default".into(),
        mission: "curator-campaign".into(),
        generation: 7,
    };
    let mut session = RunSession::spawn_mission_with_environment(
        &run_id,
        "operator",
        Some("test/model"),
        "probe",
        None,
        None,
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut observed = None;
    while std::time::Instant::now() < deadline {
        if let Some(line) = session.poll_line() {
            observed = Some(line.text);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let observed = observed.expect("fake child prints the launcher identity");
    let (mission, run) = observed.split_once('|').unwrap();
    assert_eq!(mission, "curator-campaign");
    assert_eq!(
        run,
        session.transcript.file_name().unwrap().to_string_lossy()
    );
    let _ = session.stop();
    let _ = fs::remove_dir_all(&bin);
    let _ = fs::remove_dir_all(&store_dir);
}

/// TUI backend: the pipe-pane raw capture is a durable corpus
/// artifact — it lands in the project corpus runs/ (never /tmp) and
/// survives stop/close.
#[test]
#[ignore = "platform: requires a usable tmux server"]
fn tui_raw_capture_is_durable_in_project_corpus() {
    let _guard = env_lock();
    if tmux_available().is_none() {
        return; // no tmux on this host — nothing to exercise
    }
    let _ = Command::new("pkill").args(["-f", "sleep 90128"]).status();
    let bin = unique_temp_path("corpus-fake-tui-bin");
    let _ = fs::remove_dir_all(&bin);
    fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("opencode");
    // The launched TUI stays alive, while Stop's discovery subprocess
    // answers with no conversation. That is what happens when OpenCode
    // rejects a configured model before creating a session: the raw log
    // remains the valid durable transcript and deletion must stay clean.
    fs::write(
        &fake,
        "#!/bin/sh\n\
         if [ \"$1\" = session ]; then\n\
           printf '[]\\n'\n\
           exit 0\n\
         fi\n\
         sleep 90128\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::env::var("PATH").unwrap_or_default();
    path = format!("{}:{}", bin.display(), path);
    let _path = EnvVarGuard::set("PATH", &path);

    let (store, store_dir) = tmp_store("tui-raw");
    let _store = EnvVarGuard::set("CORPUS_STORE", &store_dir);
    core_project(&store);

    let run_id = EnvironmentSessionId {
        project: "default".into(),
        mission: "curator-campaign".into(),
        generation: 9,
    };
    let mut session = RunSession::spawn_mission_with_environment(
        &run_id,
        "operator",
        Some("test/model"),
        "probe",
        None,
        None,
    )
    .expect("tui spawn");
    let (raw, script, tmux_session) = match &session.backend {
        Backend::Tui(tui) => (tui.raw.clone(), tui.script.clone(), tui.session.clone()),
        _ => panic!("expected the TUI backend (tmux is available)"),
    };
    let launch_script = fs::read_to_string(script).unwrap();
    assert!(
        launch_script.contains("export CORPUS_MISSION='curator-campaign'"),
        "{launch_script}"
    );
    assert!(
        launch_script.contains(&format!("export CORPUS_RUN_ID='{tmux_session}'")),
        "{launch_script}"
    );
    let runs_dir = store.project_corpus_dir("default").join("runs");
    assert_eq!(
        raw.parent(),
        Some(runs_dir.as_path()),
        "raw capture lives in the project corpus runs/, not /tmp"
    );
    assert_eq!(raw.extension().and_then(|e| e.to_str()), Some("raw"));

    // Simulate pane output, then stop: the run log must survive.
    fs::write(&raw, "pane output\n").unwrap();
    let outcome = session.stop_detailed();
    assert_eq!(outcome.transcript, raw);
    assert_eq!(outcome.export_error, None);
    assert!(raw.exists(), "stop keeps the durable run log");

    let _ = fs::remove_dir_all(&bin);
    let _ = fs::remove_dir_all(&store_dir);
}

/// BOTH launch paths must carry the run's agent identity in the
/// ENVIRONMENT, not just as a CLI arg: corpus-mcp inherits the env and
/// resolves the agent's role from it, so a path that omits it leaves
/// the server unable to tell a researcher from an operator. The piped
/// path used to pass `--agent` only, which is invisible to the server.
#[test]
fn both_launch_paths_export_the_agent_identity_and_tui_session() {
    let _guard = env_lock();
    let (store, store_dir) = tmp_store("agent-env");
    let _store = EnvVarGuard::set("CORPUS_STORE", &store_dir);
    // A run dir belongs to a project, and provisioning now refuses to
    // invent one.
    store.create_project("default", "D", "cdk-regtest").unwrap();

    // Piped path: the identity rides the child's environment.
    let plan = LaunchPlan::headless(
        "default",
        "discover",
        Some("test/model"),
        "probe",
        None,
        None,
        None,
    );
    let environment = LaunchEnvironment::from_plan(
        &store,
        &plan,
        BackendIdentity {
            opencode: Path::new("/bin/echo"),
            agent_stem: "discover",
            handle: "discover",
            run_log: Some("run.log"),
            run_identity: None,
            control_password: None,
        },
    );
    let command = opencode_command(Path::new("/bin/echo"), &store, &plan, &environment)
        .expect("provision the run dir");
    let exported: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(k, v)| {
            Some((
                k.to_string_lossy().into_owned(),
                v?.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    let agent_env = exported.iter().find(|(k, _)| k == AGENT_ENV);
    assert_eq!(
        agent_env.map(|(_, v)| v.as_str()),
        Some("discover"),
        "the piped path must export {AGENT_ENV}; exported: {exported:?}"
    );

    // TUI path: the identity is exported by the launch script.
    let script = std::env::temp_dir().join(format!("corpus-idscript-{}.sh", std::process::id()));
    write_tui_script(
        &script,
        &[
            (AGENT_ENV, "discover"),
            (ENVIRONMENT_SESSION_ENV, "p7-default-m5-probe-g3"),
        ],
        None,
        None,
        None,
    )
    .unwrap();
    let body = fs::read_to_string(&script).unwrap();
    assert!(
        body.contains(&format!("export {AGENT_ENV}='discover'")),
        "the TUI path must export {AGENT_ENV}: {body}"
    );
    assert!(
        body.contains(&format!(
            "export {ENVIRONMENT_SESSION_ENV}='p7-default-m5-probe-g3'"
        )),
        "the TUI path must export the durable environment session: {body}"
    );
    let _ = fs::remove_file(&script);

    let _ = fs::remove_dir_all(&store_dir);
}

/// The run script's exec line: agent+model always explicit, and
/// `--session` only when resuming an existing conversation.
#[test]
fn tui_script_carries_session_and_prompt_flags() {
    let dir = std::env::temp_dir().join(format!("corpus-script-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("run.sh");
    let read = |script: &Path| fs::read_to_string(script).unwrap();

    // Bare launch: no --session, no --prompt.
    write_tui_script(&script, &[], None, None, None).unwrap();
    let bare = read(&script);
    assert!(
        bare.contains("--agent \"$CORPUS_OPENCODE_HANDLE\""),
        "{bare}"
    );
    assert!(
        !bare.contains("--session"),
        "a fresh launch resumes nothing: {bare}"
    );
    assert!(!bare.contains("--prompt"), "{bare}");

    // App-launched runs expose one private loopback endpoint and carry
    // its password only in an owner-executable script.
    write_tui_script(
        &script,
        &[("OPENCODE_SERVER_PASSWORD", "secret")],
        None,
        None,
        Some(43_210),
    )
    .unwrap();
    let controlled = read(&script);
    assert!(
        controlled.contains("--hostname 127.0.0.1 --port 43210"),
        "{controlled}"
    );
    assert!(controlled.contains("OPENCODE_SERVER_PASSWORD='secret'"));
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o700
    );

    // Resume: the recorded id re-opens that conversation.
    write_tui_script(
        &script,
        &[],
        None,
        Some("ses_ff783a74dffeyTn76osPWIUX3L"),
        None,
    )
    .unwrap();
    let resumed = read(&script);
    assert!(
        resumed.contains("--session 'ses_ff783a74dffeyTn76osPWIUX3L'"),
        "{resumed}"
    );

    // A session id is quoted like every other dynamic value, so a
    // hostile one cannot break out of the exec line.
    write_tui_script(
        &script,
        &[],
        Some("go"),
        Some("x'; rm -rf /tmp/nope; #"),
        None,
    )
    .unwrap();
    let quoted = read(&script);
    assert!(quoted.contains(r"'x'\''; rm -rf /tmp/nope; #'"), "{quoted}");
    assert!(quoted.contains("--prompt 'go'"), "{quoted}");
    let _ = fs::remove_dir_all(&dir);
}
