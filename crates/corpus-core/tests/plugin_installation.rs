//! End-to-end local installation fixture: no repository-relative paths and
//! no Docker. This is the clean-install half of the Chunk 1 gate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use corpus_core::{Plugin, Store};

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn git(work: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(work)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn fixture_remote(root: &Path) -> (PathBuf, String) {
    let remote = root.join("target-source");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--quiet", "-b", "main"]);
    fs::write(remote.join("README.md"), "fixture target\n").unwrap();
    git(&remote, &["add", "README.md"]);
    git(
        &remote,
        &[
            "-c",
            "user.email=fixture@corpus",
            "-c",
            "user.name=fixture",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    git(&remote, &["tag", "v1.0.0"]);
    let sha = git(&remote, &["rev-parse", "HEAD"]);
    (remote, sha)
}

fn write_bundle(root: &Path, remote: &Path, sha: &str) -> PathBuf {
    let bundle = root.join("external-checkout");
    fs::create_dir_all(bundle.join("bin")).unwrap();
    fs::write(
        bundle.join("plugin.toml"),
        format!(
            r#"manifest_version = 1
id = "fixture-regtest"
version = "1.0.0"
description = "external install fixture"
protocol = "corpus.environment/1"
exec = "bin/plugin"
capabilities = ["lifecycle.setup", "sessions"]

[[sources]]
id = "target"
repo = "{}"
default_rev = "v1.0.0"
default_sha = "{}"
mount = "/opt/src/target"
"#,
            remote.display(),
            sha
        ),
    )
    .unwrap();
    let executable = bundle.join("bin/plugin");
    fs::write(
        &executable,
        r#"#!/usr/bin/env bash
set -uo pipefail
while IFS= read -r line; do
  id="$(jq -r '.id' <<<"$line")"
  method="$(jq -r '.method' <<<"$line")"
  params="$(jq -c '.params // {}' <<<"$line")"
  state="$(jq -r '.state_dir // ""' <<<"$params")"
  case "$method" in
    hello)
      printf '{"id":%s,"ok":true,"result":{"protocol":"corpus.environment/1","capabilities":["lifecycle.setup","sessions"]}}\n' "$id" ;;
    setup)
      mkdir -p "$state"
      printf partial > "$state/setup.partial"
      printf '{"id":%s,"event":"progress","phase":"dependency_fetch","message":"dependency lock verified","completed":1,"total":3}\n' "$id"
      if [[ "$(jq -r '.interrupt // false' <<<"$params")" == true ]]; then sleep 5; fi
      printf '{"id":%s,"event":"progress","phase":"image_build","message":"fixture image ready","completed":2,"total":3}\n' "$id"
      mv "$state/setup.partial" "$state/ready"
      printf '{"id":%s,"event":"progress","phase":"verification","message":"ready","completed":3,"total":3}\n' "$id"
      printf '{"id":%s,"ok":true,"result":{"ready":true}}\n' "$id" ;;
    doctor|status)
      if [[ -f "$state/ready" ]]; then
        printf '{"id":%s,"ok":true,"result":{"ready":true,"docker":{"required":false},"environment_lock":"lock:prepared","image_digest":"sha256:prepared","backbone":{"topology":"fixture-full","ownership":"owned"}}}\n' "$id"
      else
        printf '{"id":%s,"ok":false,"error":{"code":"setup_required","message":"run plugin setup","retryable":true}}\n' "$id"
      fi ;;
    operation_status)
      key="$(jq -r '.idempotency_key // ""' <<<"$params")"
      if [[ "$key" == setup:* && -f "$state/ready" ]]; then state_value=succeeded
      elif [[ "$key" == session_close:* && -f "$state/close.succeeded" ]]; then state_value=succeeded
      else state_value=unknown; fi
      jq -nc --argjson id "$id" --arg key "$key" --arg state "$state_value" \
        '{id:$id,ok:true,result:{idempotency_key:$key,state:$state}}' ;;
    session_open)
      mkdir -p "$state"
      jq -c '.sources' <<<"$params" > "$state/sources.json"
      touch "$state/resources-live"
      printf '{"id":%s,"ok":true,"result":{"environment_lock":"fixture-lock","image_digest":"sha256:fixture"}}\n' "$id" ;;
    session_probe)
      printf '{"id":%s,"ok":true,"result":{"ready":true,"notes":"fixture session ready"}}\n' "$id" ;;
    describe)
      printf '{"id":%s,"ok":true,"result":{"targets":[{"id":"target","kind":"fixture","url":"http://fixture:8080","source_id":"target","source_sha":"PLACEHOLDER"}],"tools":[],"limits":{},"provenance":{}}}\n' "$id" ;;
    session_close)
      if [[ -f "$state/fail-close-once" ]]; then
        rm -f "$state/fail-close-once"
        printf '{"id":%s,"ok":false,"error":{"code":"cleanup_failed","message":"injected close failure","retryable":true}}\n' "$id"
      else
        rm -f "$state/resources-live"
        touch "$state/close.succeeded"
        printf 'closed\n' >> "$state/close.calls"
        printf '{"id":%s,"ok":true,"result":{"closed":true}}\n' "$id"
      fi ;;
    stop)
      if [[ -s "$state/leases" ]]; then
        printf '{"id":%s,"ok":false,"error":{"code":"sessions_active","message":"active leases refuse stop","retryable":true}}\n' "$id"
      else
        printf '{"id":%s,"ok":true,"result":{"stopped":true}}\n' "$id"
      fi ;;
    *) printf '{"id":%s,"ok":false,"error":{"code":"unknown_method","message":"unknown method","retryable":false}}\n' "$id" ;;
  esac
done
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bundle
}

#[test]
fn external_read_only_bundle_recovers_setup_and_populates_sources() {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "corpus-installed-fixture-{}-{stamp}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let _home = EnvGuard::set("CORPUS_HOME", root.join("home"));
    let _plugins = EnvGuard::set("CORPUS_PLUGINS_DIR", "");
    let _sources = EnvGuard::set("CORPUS_SOURCES_DIR", root.join("home/cache/sources"));

    let (remote, sha) = fixture_remote(&root);
    let bundle = write_bundle(&root, &remote, &sha);
    let receipt = corpus_core::install_plugin_bundle(&bundle).unwrap();
    let selected = corpus_core::find_plugin("fixture-regtest")
        .unwrap()
        .unwrap();
    assert_eq!(selected.dir, receipt.path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&selected.dir).unwrap().permissions().mode() & 0o222,
            0
        );
    }

    let mut interrupted_params = corpus_core::plugin_lifecycle_params(&selected).unwrap();
    interrupted_params["interrupt"] = serde_json::Value::Bool(true);
    let mut interrupted = Plugin::spawn(&selected.dir).unwrap();
    let started = Instant::now();
    let error = interrupted
        .lifecycle_call_cancellable(
            "setup",
            Some(interrupted_params),
            Duration::from_secs(2),
            || started.elapsed() > Duration::from_millis(100),
            |_| {},
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled"), "{error}");

    let params = corpus_core::plugin_lifecycle_params(&selected).unwrap();
    let mut phases = Vec::new();
    let mut setup = Plugin::spawn(&selected.dir).unwrap();
    setup
        .lifecycle_call(
            "setup",
            Some(params.clone()),
            Duration::from_secs(2),
            |progress| phases.push(progress.phase.clone()),
        )
        .unwrap();
    assert_eq!(phases, ["dependency_fetch", "image_build", "verification"]);
    let mut repeated_phases = Vec::new();
    let repeated = corpus_core::call_plugin_lifecycle_cancellable(
        &selected,
        "setup",
        Duration::from_secs(2),
        || false,
        |progress| repeated_phases.push(progress.phase.clone()),
    )
    .unwrap();
    assert_eq!(repeated, serde_json::Value::Null);
    assert!(
        repeated_phases.is_empty(),
        "succeeded setup is not run twice"
    );
    let mut doctor = Plugin::spawn(&selected.dir).unwrap();
    assert_eq!(
        doctor
            .lifecycle_call("doctor", Some(params), Duration::from_secs(2), |_| {})
            .unwrap()["ready"],
        true
    );
    let status = corpus_core::selected_plugin_status(Some("fixture-regtest"));
    let selected_status = status
        .iter()
        .find(|candidate| candidate.name == "fixture-regtest")
        .unwrap();
    assert!(
        selected_status.probed && selected_status.ready,
        "{selected_status:?}"
    );
    assert_eq!(
        selected_status.protocol.as_deref(),
        Some(corpus_core::ENVIRONMENT_PROTOCOL_V1)
    );
    assert_eq!(
        selected_status.capabilities,
        ["lifecycle.setup", "sessions"]
    );
    assert_eq!(selected_status.origin, corpus_core::PluginOrigin::Installed);
    assert_eq!(
        selected_status.bundle_digest.as_deref(),
        Some(receipt.digest.as_str())
    );
    assert_eq!(selected_status.prepared.docker_required, Some(false));
    assert_eq!(
        selected_status.prepared.environment_lock.as_deref(),
        Some("lock:prepared")
    );
    assert_eq!(
        selected_status.prepared.image_digest.as_deref(),
        Some("sha256:prepared")
    );
    assert_eq!(
        selected_status.prepared.topology.as_deref(),
        Some("fixture-full")
    );
    assert_eq!(
        selected_status.prepared.backbone_ownership.as_deref(),
        Some("owned")
    );

    let store = Store::new(root.join("home/store"));
    store.create_project("p", "P", "fixture-regtest").unwrap();
    let sources = corpus_core::plugin_sources(&store, "p").unwrap();
    assert_eq!(sources[0].name, "target");
    let mut pins = BTreeMap::new();
    pins.insert("target".to_string(), "v1.0.0".to_string());
    assert_eq!(
        corpus_core::prepare_source_pins(&store, "p", &pins).unwrap()["target"],
        sha
    );
    assert!(store
        .source_cache_dir()
        .join("target")
        .join(&sha)
        .join(".git")
        .is_dir());

    let id = corpus_core::EnvironmentSessionId {
        project: "p".into(),
        mission: "fixture-mission".into(),
        generation: 1,
    };
    let mut environment = corpus_core::open_environment_session(
        &store,
        id.clone(),
        BTreeMap::from([("target".into(), sha.clone())]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        environment.state,
        corpus_core::EnvironmentSessionState::Ready
    );
    assert_eq!(
        environment.environment_lock.as_deref(),
        Some("fixture-lock")
    );
    assert_eq!(
        store
            .load_environment_session("fixture-regtest", &id)
            .unwrap()
            .image_digest
            .as_deref(),
        Some("sha256:fixture")
    );
    let mut session_plugin = Plugin::spawn(&selected.dir).unwrap();
    session_plugin.hello().unwrap();
    let description = session_plugin.describe_v1(&id.storage_key()).unwrap();
    assert_eq!(description.targets[0].id, "target");
    let state_dir = store
        .plugin_runtime_dir("fixture-regtest")
        .unwrap()
        .join("state")
        .join(id.storage_key());
    fs::write(state_dir.join("fail-close-once"), "retry me").unwrap();
    let close_error = corpus_core::close_environment_session(&store, &mut environment)
        .unwrap_err()
        .to_string();
    assert!(
        close_error.contains("injected close failure"),
        "{close_error}"
    );
    assert_eq!(
        environment.state,
        corpus_core::EnvironmentSessionState::Failed
    );
    corpus_core::close_environment_session_key(&store, "fixture-regtest", &id.storage_key())
        .unwrap();
    environment = store
        .load_environment_session("fixture-regtest", &id)
        .unwrap();
    assert_eq!(
        environment.state,
        corpus_core::EnvironmentSessionState::Closed
    );
    assert!(environment.cleanup_verified_at.is_some());

    // A late opener can recreate resources after an older successful close.
    // The old close receipt is not proof of current physical state: closing
    // again must invoke the idempotent plugin cleanup instead of replaying
    // success and leaving the resource behind.
    fs::write(state_dir.join("resources-live"), "late open").unwrap();
    corpus_core::close_environment_session_key(&store, "fixture-regtest", &id.storage_key())
        .unwrap();
    assert!(!state_dir.join("resources-live").exists());
    assert_eq!(
        fs::read_to_string(state_dir.join("close.calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );

    let resolved_json =
        serde_json::to_string(&BTreeMap::from([("target".to_string(), sha.clone())])).unwrap();
    let run = store
        .provision_run_dir_with_sources("p", Some(&resolved_json))
        .unwrap();
    assert_eq!(
        fs::read_link(run.join("sources/target").join(&sha)).unwrap(),
        store
            .source_cache_dir()
            .join("target")
            .join(&sha)
            .canonicalize()
            .unwrap()
    );
    let _ = fs::remove_dir_all(&root);
}
