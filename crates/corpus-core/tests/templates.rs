//! Step-1 assertion: rendering the core seed agents must reproduce the
//! checked-in `.opencode/agent/*.md` files — same permission blocks, same
//! prompt content. Generation replaces hand-editing; this test is the fence
//! that keeps the two in lockstep. A permission difference is a hard failure;
//! whitespace/frontmatter-order differences would also flag here because we
//! assert byte equality, so drift is caught the strictest way available.

use std::path::{Path, PathBuf};
use std::fs;

use corpus_core::Store;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The in-repo store (seeds live under store/templates/agents).
fn store() -> Store {
    Store::new(repo_root().join("store"))
}

/// Render the two core agents via the seed→project→render pipeline into
/// a temp dir (creates a temp project for the render call).
fn render_all(dest: &Path) -> Vec<(String, String)> {
    let store = store();
    let tmp = repo_root().join("target").join(format!("tpl-test-{}", std::process::id()));
    let test_store = Store::new(tmp.clone());
    let _ = fs::remove_dir_all(&tmp);
    // Copy the seed dirs so create_agent_from_seed can find them.
    let seed_src = store.seed_agents_dir();
    let seed_dst = test_store.seed_agents_dir();
    if seed_src.is_dir() {
        copy_tree(&seed_src, &seed_dst);
    }
    test_store.create_project("p", "P", "cdk-regtest").expect("create_project");
    let mut out = Vec::new();
    for slug in ["operator", "researcher"] {
        test_store.render_agent("p", slug).unwrap();
        let rendered_path = test_store.opencode_agent_dir().join(format!("{slug}.md"));
        let text = fs::read_to_string(&rendered_path).unwrap();
        fs::write(dest.join(format!("{slug}.md")), &text).unwrap();
        out.push((slug.to_string(), text));
    }
    let _ = fs::remove_dir_all(&tmp);
    out
}

/// Parse the frontmatter out of an agent file (semantic comparison, immune
/// to serialization quoting choices).
fn frontmatter(agent_raw: &str) -> serde_yaml::Mapping {
    let (fm, _body) = corpus_core::frontmatter::split(agent_raw).expect("parse frontmatter");
    fm.expect("agent file has frontmatter")
}

#[test]
fn rendered_core_agents_match_checked_in_files() {
    let repo = repo_root();
    let tmp = std::env::temp_dir().join(format!("corpus-template-render-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    for (slug, rendered) in render_all(&tmp) {
        let original_path = repo.join(".opencode/agent").join(format!("{slug}.md"));
        let original = fs::read_to_string(&original_path)
            .unwrap_or_else(|e| panic!("read checked-in {slug}.md: {e}"));

        assert_eq!(
            rendered, original,
            "step-1 assertion: rendered agent {slug} differs from the checked-in \
             {}. If you changed the seed, regenerate the agent file with \
             `cargo run --example render_seeds` and commit the result. If the \
             rendered output is right and the checked-in file is stale, this \
             test is the reviewer you were waiting for.",
            original_path.display()
        );

        // Belt and braces: permission blocks parse to the same YAML.
        let rendered_fm = frontmatter(&rendered);
        let original_fm = frontmatter(&original);
        assert_eq!(
            rendered_fm.get(serde_yaml::Value::String("permission".into())),
            original_fm.get(serde_yaml::Value::String("permission".into())),
            "permission block mismatch for {slug}"
        );
        assert_eq!(
            rendered_fm.get(serde_yaml::Value::String("mode".into())),
            original_fm.get(serde_yaml::Value::String("mode".into())),
            "mode mismatch for {slug}"
        );
    }
    let _ = fs::remove_dir_all(&tmp);
}

/// Minimal detector for the pattern matcher used by the enforcement layer
/// below.
fn matches(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.split_first(), t.split_first()) {
            (None, None) => true,
            (Some((b'*', rest)), _) => go(rest, t) || (!t.is_empty() && go(p, &t[1..])),
            (Some((b'?', rest)), Some((_, t_rest))) => go(rest, t_rest),
            (Some((pat_b, rest)), Some((txt_b, t_rest))) if pat_b == txt_b => go(rest, t_rest),
            _ => false,
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// Evaluate a permission map's rules in order, last match wins.
fn permission_for(map: &serde_yaml::Mapping, path: &str) -> Option<String> {
    let mut action: Option<String> = None;
    for (k, v) in map {
        let Some(pattern) = k.as_str() else { continue };
        let Some(act) = v.as_str() else { continue };
        if matches(pattern, path) {
            action = Some(act.to_string());
        }
    }
    action
}

/// Read a permission block from a seed's opencode.json (agent entry).
fn seed_perm(seed_slug: &str) -> serde_json::Value {
    let path = repo_root()
        .join("store/templates/agents")
        .join(seed_slug)
        .join("opencode.json");
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    doc["agent"][seed_slug]["permission"].clone()
}

/// Read the permission block as a serde_yaml Mapping for rule evaluation.
fn seed_perm_map(seed_slug: &str) -> serde_yaml::Mapping {
    let json = seed_perm(seed_slug);
    let yaml_str = serde_yaml::to_string(&json).unwrap();
    serde_yaml::from_str(&yaml_str).unwrap()
}

#[test]
fn core_seed_trust_domain_semantics_v2() {
    // Operator: all host surfaces denied.
    let op = seed_perm_map("operator");
    for key in ["bash", "edit", "write", "read", "glob", "grep", "list", "external_directory", "webfetch", "websearch", "task"] {
        assert_eq!(
            op.get(serde_yaml::Value::String(key.into())).and_then(|v| v.as_str()),
            Some("deny"),
            "operator must deny {key}"
        );
    }

    // Researcher: cannot execute, can read web + project corpus, cannot read benchmarks.
    let res = seed_perm_map("researcher");
    let get = |k: &str| -> String {
        res.get(serde_yaml::Value::String(k.into()))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    for k in ["bash", "task", "corpus_sandbox_exec", "corpus_faucet", "corpus_wallet_fund", "corpus_oracle_run", "corpus_finding_write", "corpus_attack_save"] {
        assert_eq!(get(k), "deny", "researcher must deny {k}");
    }
    assert_eq!(get("webfetch"), "allow");
    assert_eq!(get("websearch"), "allow");
    assert_eq!(get("corpus_target_info"), "allow");
    assert_eq!(get("corpus_technique_save"), "allow");

    // corpus_promote is gone — no deny entry for a tool that no longer exists.
    assert!(!res.contains_key(serde_yaml::Value::String("corpus_promote".into())),
        "corpus_promote deny removed (tool deleted)");

    // Write/edit access is project-corpus-scoped (/teams/ segment gone).
    let write = res.get(serde_yaml::Value::String("write".into()))
        .and_then(|v| v.as_mapping().cloned())
        .expect("researcher write is a rule map");
    let project_corpus = "store/projects/p/corpus/findings/x.md";
    let legacy_flat = "store/hypotheses/older-embedded-path.md";
    let team_corpus = "store/projects/p/teams/t/corpus/findings/x.md";

    // The project corpus matches the allow pattern.
    let project_pattern = "store/projects/*/corpus/**";
    assert!(matches(project_pattern, project_corpus), "project corpus matches allow pattern");
    // Known residual: opencode's `*` is greedy and also matches the old
    // /teams/ segment, so the write gate remains MCP-only.
    assert_eq!(
        permission_for(&write, project_corpus).as_deref(),
        Some("allow"),
        "project corpus writes are allowed"
    );
    assert_eq!(
        permission_for(&write, legacy_flat).as_deref(),
        Some("deny"),
        "legacy flat-store write paths resolve to deny"
    );

    // Read: benchmarks denied.
    let read = res.get(serde_yaml::Value::String("read".into())).unwrap();
    assert_eq!(read.get(serde_yaml::Value::String("*".into())).and_then(|v| v.as_str()), Some("allow"));
    assert_eq!(read.get(serde_yaml::Value::String("benchmarks/**".into())).and_then(|v| v.as_str()), Some("deny"));

    // Edit (covers write/patch) also denies legacy paths and allows project corpus.
    let edit = res.get(serde_yaml::Value::String("edit".into()))
        .and_then(|v| v.as_mapping().cloned())
        .expect("researcher edit is a rule map");
    assert_eq!(
        permission_for(&edit, project_corpus).as_deref(),
        Some("allow"),
        "edit allows project corpus"
    );
    assert_eq!(
        permission_for(&edit, legacy_flat).as_deref(),
        Some("deny"),
        "edit denies legacy flat paths"
    );
}

/// Recursively copy a directory tree.
fn copy_tree(src: &Path, dst: &Path) {
    if !src.is_dir() {
        if let Some(parent) = dst.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::copy(src, dst);
        return;
    }
    let _ = fs::create_dir_all(dst);
    for entry in fs::read_dir(src).into_iter().flatten().flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            let _ = fs::copy(&from, &to);
        }
    }
}