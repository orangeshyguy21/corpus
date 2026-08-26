//! Opt-in live baseline for the selected-plugin and pre-spawn launch paths.
//!
//! Run from a Docker-capable host with:
//! `cargo test -p corpus-core --test plugin_baseline -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use corpus_core::{AgentRole, Store};

const SAMPLES: usize = 5;

struct SourcesGuard(Option<std::ffi::OsString>);

impl SourcesGuard {
    fn warm_workspace_cache() -> Self {
        let previous = std::env::var_os("CORPUS_SOURCES_DIR");
        let warm = corpus_core::resource_root().unwrap().join("sources");
        std::env::set_var("CORPUS_SOURCES_DIR", warm);
        Self(previous)
    }
}

impl Drop for SourcesGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(previous) => std::env::set_var("CORPUS_SOURCES_DIR", previous),
            None => std::env::remove_var("CORPUS_SOURCES_DIR"),
        }
    }
}

fn milliseconds(samples: &[Duration]) -> Vec<u128> {
    samples.iter().map(Duration::as_millis).collect()
}

fn summary(label: &str, samples: &[Duration]) {
    let values = milliseconds(samples);
    let mut sorted = values.clone();
    sorted.sort_unstable();
    let total: u128 = values.iter().sum();
    let mean = total / values.len() as u128;
    let p95 = sorted[(sorted.len() * 95).div_ceil(100).saturating_sub(1)];
    println!(
        "{label}: samples_ms={values:?} mean_ms={mean} p95_ms={p95} max_ms={}",
        sorted.last().copied().unwrap_or_default()
    );
}

fn command_output(command: &str, args: &[&str]) -> String {
    std::process::Command::new(command)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[test]
#[ignore = "live: requires the installed CDK plugin, its source cache, and Docker"]
fn selected_probe_and_warm_launch_preparation() {
    let _sources = SourcesGuard::warm_workspace_cache();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "corpus-plugin-baseline-{}-{stamp}",
        std::process::id()
    ));
    let store = Store::new(root.join("store"));
    store
        .create_project("baseline", "Plugin baseline", "cdk-regtest")
        .unwrap();
    store
        .create_agent_with_role("baseline", "tester", AgentRole::Tester)
        .unwrap();

    // Pin the audited defaults. Enumerating before the timer intentionally
    // makes this the warm source/revision-cache launch-preparation fixture.
    let sources = corpus_core::plugin_sources(&store, "baseline").unwrap();
    let pins: BTreeMap<String, String> = sources
        .into_iter()
        .map(|source| (source.name, source.pinned))
        .collect();
    assert!(!pins.is_empty(), "cdk-regtest declares no sources");

    println!(
        "fixture: commit={} profile={} os={} cpus={} samples={} source_cache=warm",
        command_output("git", &["rev-parse", "HEAD"]),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        command_output("uname", &["-a"]),
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or_default(),
        SAMPLES,
    );

    let mut probe_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let statuses = corpus_core::selected_plugin_status(Some("cdk-regtest"));
        probe_samples.push(started.elapsed());
        let selected = statuses
            .iter()
            .find(|status| status.name == "cdk-regtest")
            .expect("selected plugin remains discoverable");
        assert!(selected.probed, "selected plugin was not probed");
        assert!(
            selected.ready,
            "selected plugin is not ready: {}",
            selected.notes
        );
    }

    let mut preparation_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let resolved = corpus_core::prepare_source_pins(&store, "baseline", &pins).unwrap();
        store.load_agent("baseline", "tester").unwrap();
        store.render_project_agents("baseline").unwrap();
        preparation_samples.push(started.elapsed());
        assert_eq!(resolved.len(), pins.len());
    }

    summary("selected_plugin_probe", &probe_samples);
    summary("warm_mission_preparation", &preparation_samples);
    let _ = std::fs::remove_dir_all(root);
}
