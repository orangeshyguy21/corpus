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
fn project_commands_round_trip_through_the_binary() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-project-contract-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);

    assert_eq!(
        success(
            &root,
            &["project", "new", "alpha", "--name", "Alpha", "--plugin", "fixture"]
        ),
        "created project alpha (plugin: fixture)\n"
    );
    let listed = success(&root, &["project", "list"]);
    assert!(listed.contains("alpha"), "{listed}");
    assert!(listed.contains("plugin=fixture"), "{listed}");

    assert_eq!(
        success(
            &root,
            &[
                "project",
                "clone",
                "alpha",
                "--to",
                "beta",
                "--name",
                "Beta",
                "--with-corpus"
            ]
        ),
        "cloned project alpha -> beta\n"
    );
    assert_eq!(
        success(&root, &["project", "rebind", "beta", "--plugin", "other"]),
        "rebound project beta -> plugin other\n"
    );
    assert_eq!(
        success(&root, &["project", "wipe", "beta"]),
        "wiped project corpus beta (generation 1)\n"
    );
    assert_eq!(
        success(&root, &["project", "delete", "beta"]),
        "deleted project beta\n"
    );

    let listed = success(&root, &["project", "list"]);
    assert!(listed.contains("alpha"), "{listed}");
    assert!(!listed.contains("beta"), "{listed}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_required_options_fail_before_store_creation() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-project-validation-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let output = corpus(&root, &["project", "clone", "alpha"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("corpus: error:"), "{stderr}");
    assert!(stderr.contains("--to <TO>"), "{stderr}");
    assert!(!root.exists(), "parser failure created {}", root.display());
}
