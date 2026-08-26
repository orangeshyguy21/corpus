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

#[test]
fn probe_migration_is_dry_run_first_through_the_binary() {
    let root =
        std::env::temp_dir().join(format!("corpus-cli-probe-migration-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    success(&root, &["project", "new", "alpha", "--plugin", "fixture"]);
    let corpus = root.join("store/projects/alpha/corpus");
    fs::remove_dir(corpus.join("probes")).unwrap();
    fs::create_dir_all(corpus.join("attacks/replay")).unwrap();
    fs::write(corpus.join("attacks/replay/attack.md"), "legacy\n").unwrap();
    fs::write(corpus.join("attacks/replay/run.sh"), "#!/bin/sh\n").unwrap();

    let preview = success(&root, &["project", "migrate-probes", "alpha"]);
    assert!(preview.contains("dry run"), "{preview}");
    assert!(preview.contains("re-run with --apply"), "{preview}");
    assert!(corpus.join("attacks/replay/attack.md").is_file());

    let applied = success(&root, &["project", "migrate-probes", "alpha", "--apply"]);
    assert!(applied.contains("applied"), "{applied}");
    assert!(corpus.join("probes/replay/probe.md").is_file());
    assert!(!corpus.join("attacks").exists());
    assert_eq!(
        success(&root, &["project", "migrate-probes", "alpha", "--apply"]),
        "project alpha: probe namespace already current\n"
    );
    let _ = fs::remove_dir_all(root);
}
