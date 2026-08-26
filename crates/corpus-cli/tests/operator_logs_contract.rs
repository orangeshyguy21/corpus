use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use corpus_core::audit::{self, AuditRecord, Outcome};
use corpus_core::refusal::{self, Gate, RefusalRecord};
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
fn operator_logs_tail_filter_and_render_through_the_binary() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-operator-logs-contract-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = Store::new(root.join("store"));

    assert!(success(&root, &["audit", "empty"]).contains("no recorded changes for empty"));
    let empty_refusals = success(&root, &["refusals", "empty", "--gate", "role"]);
    assert!(empty_refusals.contains("no refusals recorded for empty at gate role"));
    assert!(empty_refusals.contains("nothing the corpus server refused"));

    for target in ["agents/first", "agents/second", "agents/third"] {
        audit::append(
            &store,
            "p",
            &AuditRecord::new(
                "curator:keeper",
                "agent_set_role",
                target,
                Outcome::Ok,
                "line one\nline two\nline three\nline four",
            ),
        )
        .unwrap();
    }
    let audit_output = success(&root, &["audit", "p", "--tail", "2"]);
    assert!(!audit_output.contains("agents/first"), "{audit_output}");
    assert!(audit_output.contains("agents/second"), "{audit_output}");
    assert!(audit_output.contains("agents/third"), "{audit_output}");
    assert!(!audit_output.contains("line four"), "{audit_output}");

    let mut role = RefusalRecord::new("sandbox_exec", Gate::Role, "role denied");
    role.actor = "curator:keeper".into();
    role.role = Some("researcher".into());
    role.run_log = Some("100-worker.raw".into());
    role.args = r#"{"command":"pwd"}"#.into();
    refusal::record(&store, Some("p"), &role);
    refusal::record(
        &store,
        Some("p"),
        &RefusalRecord::new("finding_write", Gate::Scope, "scope denied"),
    );

    let refusal_output = success(&root, &["refusals", "p", "--gate", "role"]);
    assert!(refusal_output.contains("sandbox_exec"), "{refusal_output}");
    assert!(refusal_output.contains("researcher"), "{refusal_output}");
    assert!(
        refusal_output.contains("run=100-worker.raw"),
        "{refusal_output}"
    );
    assert!(
        refusal_output.contains(r#"args: {"command":"pwd"}"#),
        "{refusal_output}"
    );
    assert!(
        !refusal_output.contains("finding_write"),
        "{refusal_output}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_gate_fails_before_store_creation() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-operator-logs-validation-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let output = corpus(&root, &["refusals", "p", "--gate", "permission"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("corpus: error:"), "{stderr}");
    assert!(stderr.contains("identity, role, scope"), "{stderr}");
    assert!(!root.exists(), "parser failure created {}", root.display());
}
