use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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
fn agent_commands_round_trip_through_the_binary() {
    let root =
        std::env::temp_dir().join(format!("corpus-cli-agent-contract-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    success(&root, &["project", "new", "p", "--plugin", "fixture"]);

    assert_eq!(
        success(&root, &["agent", "new", "p", "worker", "--role", "tester"]),
        "created agent p/worker (role: tester)\n"
    );
    let listed = success(&root, &["agent", "list", "p"]);
    assert!(listed.contains("worker"), "{listed}");
    assert_eq!(
        success(&root, &["agent", "role", "p", "worker"]),
        "p/worker: tester\n"
    );
    assert_eq!(
        success(&root, &["agent", "role", "p", "worker", "curator"]),
        "p/worker: role -> curator\n"
    );
    assert_eq!(
        success(&root, &["agent", "clone", "p", "worker", "--to", "copy"]),
        "cloned agent p/worker -> copy\n"
    );
    let migration = success(&root, &["agent", "migrate-roles", "p"]);
    assert!(migration.contains("dry run"), "{migration}");
    assert!(migration.contains("already assigned"), "{migration}");
    assert_eq!(
        success(&root, &["agent", "delete", "p", "copy"]),
        "deleted agent p/copy\n"
    );

    let listed = success(&root, &["agent", "list", "p"]);
    assert!(listed.contains("worker"), "{listed}");
    assert!(!listed.contains("copy"), "{listed}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_role_fails_before_store_creation() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-agent-validation-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let output = corpus(&root, &["agent", "new", "p", "worker", "--role", "admin"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("corpus: error:"), "{stderr}");
    assert!(
        stderr.contains("super|curator|tester|researcher"),
        "{stderr}"
    );
    assert!(!root.exists(), "parser failure created {}", root.display());
}
