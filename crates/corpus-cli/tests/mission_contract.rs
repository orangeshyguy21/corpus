use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use corpus_core::Store;

fn corpus(data_root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_corpus"))
        .args(args)
        .env("CORPUS_HOME", data_root)
        .env_remove("CORPUS_STORE")
        .output()
        .expect("run corpus binary")
}

fn success(data_root: &Path, args: &[&str]) -> String {
    let output = corpus(data_root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn mission_commands_round_trip_through_the_binary() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-mission-contract-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    success(&root, &["project", "new", "p", "--plugin", "fixture"]);
    success(&root, &["agent", "new", "p", "worker", "--role", "tester"]);

    assert_eq!(
        success(
            &root,
            &[
                "mission",
                "new",
                "p",
                "probe",
                "--agent",
                "worker",
                "--budget",
                "20m",
                "--pin",
                "target=main",
                "inspect",
                "the",
                "parser",
                "--pin",
                "tools=v2",
            ],
        ),
        "created mission p/probe\n"
    );
    let listed = success(&root, &["mission", "list", "p"]);
    assert!(listed.contains("probe"), "{listed}");
    assert!(listed.contains("agent=worker"), "{listed}");
    assert!(listed.contains("budget=20m"), "{listed}");
    assert!(listed.contains("\"target\": \"main\""), "{listed}");
    assert!(listed.contains("\"tools\": \"v2\""), "{listed}");

    let store = Store::new(root.join("store"));
    assert_eq!(
        store.mission_brief("p", "probe").unwrap(),
        "\ninspect the parser"
    );
    assert_eq!(
        success(&root, &["mission", "delete", "p", "probe"]),
        "deleted mission p/probe\n"
    );

    success(
        &root,
        &["mission", "new", "p", "live", "--agent", "worker", "brief"],
    );
    let mut live = store.load_mission("p", "live").unwrap();
    live.session = Some("corpus-live".into());
    store.update_mission("p", "live", &live).unwrap();
    assert_eq!(
        success(&root, &["mission", "delete", "p", "live"]),
        "deletion requested for mission p/live; open corpus-app to complete lifecycle teardown\n"
    );
    let retained = store.load_mission("p", "live").unwrap();
    assert_eq!(retained.session.as_deref(), Some("corpus-live"));
    assert!(retained.delete_requested.is_some());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn malformed_mission_fails_before_store_creation() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-mission-validation-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let output = corpus(
        &root,
        &[
            "mission", "new", "p", "probe", "--agent", "worker", "--pin", "main", "brief",
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("corpus: error:"), "{stderr}");
    assert!(stderr.contains("source=revision"), "{stderr}");
    assert!(!root.exists(), "parser failure created {}", root.display());
}
