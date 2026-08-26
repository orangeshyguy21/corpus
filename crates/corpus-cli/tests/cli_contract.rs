use std::process::Command;

fn corpus(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_corpus"))
        .args(args)
        .env_remove("CORPUS_PROJECT")
        .output()
        .expect("run corpus binary")
}

#[test]
fn bare_and_flag_help_name_the_binary_and_headless_surface() {
    for args in [&[][..], &["--help"][..]] {
        let output = corpus(args);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage: corpus <COMMAND>"), "{stdout}");
        assert!(stdout.contains("run"), "{stdout}");
        assert!(stdout.contains("plugin"), "{stdout}");
        assert!(stdout.contains("CORPUS_PROJECT"), "{stdout}");
        assert!(!stdout.contains("corpus-cli"), "{stdout}");
        assert!(!stdout.contains("corpus [tui]"), "{stdout}");
    }
}

#[test]
fn unknown_commands_keep_the_corpus_error_prefix_and_failure_exit() {
    let output = corpus(&["nonesuch"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("corpus: error: unknown command: nonesuch"),
        "{stderr}"
    );
    assert!(stderr.contains("Usage: corpus <COMMAND>"), "{stderr}");
}

#[test]
fn malformed_run_is_rejected_before_project_or_model_work() {
    let output = corpus(&["run", "researcher"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("corpus: error:"), "{stderr}");
    assert!(stderr.contains("<MISSION>..."), "{stderr}");
    assert!(!stderr.contains("CORPUS_PROJECT is required"), "{stderr}");
}

#[test]
fn plugin_and_models_expose_generated_nested_help() {
    for (args, expected) in [
        (
            &["plugin", "--help"][..],
            &["install", "doctor", "probe", "call"][..],
        ),
        (&["models", "--help"][..], &["list"][..]),
    ] {
        let output = corpus(args);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        for item in expected {
            assert!(stdout.contains(item), "missing {item:?} in {stdout}");
        }
    }
}

#[test]
fn malformed_plugin_arguments_fail_before_catalog_discovery() {
    let output = corpus(&["plugin", "call", "fixture"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("corpus: error:"), "{stderr}");
    assert!(stderr.contains("<METHOD>"), "{stderr}");
    assert!(!stderr.contains("plugin not found"), "{stderr}");
}
