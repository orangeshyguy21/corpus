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
fn finding_commands_filter_and_read_through_the_binary() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-finding-contract-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    success(&root, &["project", "new", "p", "--plugin", "fixture"]);
    let findings = root.join("store/projects/p/corpus/findings");
    fs::write(
        findings.join("100-high.md"),
        "---\ntitle: Mint overflow\nseverity: high\ntimestamp: 100\nid: HIGH-1\n---\n\nExact high body\n",
    )
    .unwrap();
    fs::write(
        findings.join("200-low.md"),
        "---\ntitle: Minor parser issue\nseverity: low\ntimestamp: 200\nid: LOW-1\n---\n\nLow body\n",
    )
    .unwrap();
    fs::write(findings.join("plain.md"), "# Unrated note\n").unwrap();

    let listed = success(&root, &["finding", "list", "p"]);
    assert!(
        listed.starts_with("SEVERITY\tTIMESTAMP\tREFERENCE"),
        "{listed}"
    );
    assert!(
        listed.contains("HIGH\t100\tHIGH-1\tMint overflow"),
        "{listed}"
    );
    assert!(
        listed.contains("LOW\t200\tLOW-1\tMinor parser issue"),
        "{listed}"
    );
    assert!(listed.contains("UNRATED"), "{listed}");

    let filtered = success(
        &root,
        &[
            "finding",
            "list",
            "p",
            "--severity",
            "critical,high",
            "--severity",
            "high",
            "--exclude-unrated",
            "--text",
            "mint",
            "--sort",
            "severity",
            "--limit",
            "1",
        ],
    );
    assert!(filtered.contains("HIGH-1"), "{filtered}");
    assert!(!filtered.contains("LOW-1"), "{filtered}");
    assert!(!filtered.contains("UNRATED"), "{filtered}");
    assert_eq!(
        success(
            &root,
            &["finding", "list", "p", "--text", "no such finding"]
        ),
        "(no matching findings) p\n"
    );

    let expected = fs::read_to_string(findings.join("100-high.md")).unwrap();
    assert_eq!(
        success(&root, &["finding", "show", "p", "findings/100-high.md"]),
        expected
    );

    let escaped = corpus(&root, &["finding", "show", "p", "../project.yaml"]);
    assert!(!escaped.status.success());
    let stderr = String::from_utf8_lossy(&escaped.stderr);
    assert!(stderr.contains("beginning findings/"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_filter_fails_before_store_creation() {
    let root = std::env::temp_dir().join(format!(
        "corpus-cli-finding-validation-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let output = corpus(&root, &["finding", "list", "p", "--severity", "urgent"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("corpus: error:"), "{stderr}");
    assert!(
        stderr.contains("critical, high, medium, or low"),
        "{stderr}"
    );
    assert!(!root.exists(), "parser failure created {}", root.display());
}
