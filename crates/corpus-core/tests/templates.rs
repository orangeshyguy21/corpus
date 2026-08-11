//! Step-1 assertion: rendering the core agent templates must reproduce the
//! checked-in `.opencode/agent/*.md` files — same permission blocks, same
//! prompt content. Generation replaces hand-editing; this test is the fence
//! that keeps the two in lockstep. A permission difference is a hard failure;
//! whitespace/frontmatter-order differences would also flag here because we
//! assert byte equality, so drift is caught the strictest way available.

use std::path::{Path, PathBuf};

use corpus_core::{AgentTemplate, Store, Templates};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The in-repo store (core templates live under store/templates).
fn store() -> Store {
    Store::new(repo_root().join("store"))
}

/// Render the two core core agents into a temp dir.
fn render_all(dest: &Path) -> Vec<(String, String)> {
    let store = store();
    let core: Templates = store.core_templates();
    // A project-templates dir is not required when nothing shadows core.
    let dummy = std::env::temp_dir().join("corpus-render-empty");
    let local = Templates::at(&dummy);
    let mut out = Vec::new();
    for slug in ["operator", "researcher"] {
        let agent = AgentTemplate::load(&core.agents, slug)
            .unwrap_or_else(|e| panic!("load core agent {slug}: {e}"));
        let dest_path = dest.join(format!("{slug}.md"));
        agent.render(&local, &core, None, &dest_path).unwrap();
        out.push((
            slug.to_string(),
            std::fs::read_to_string(&dest_path).unwrap(),
        ));
    }
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
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    for (slug, rendered) in render_all(&tmp) {
        let original_path = repo.join(".opencode/agent").join(format!("{slug}.md"));
        let original = std::fs::read_to_string(&original_path)
            .unwrap_or_else(|e| panic!("read checked-in {slug}.md: {e}"));

        assert_eq!(
            rendered, original,
            "step-1 assertion: rendered agent {slug} differs from the checked-in \
             {}. If you changed the template, regenerate the agent file with \
             `corpus template render {slug}` and commit both. If the rendered \
             output is right and the checked-in file is stale, this test is the \
             reviewer you were waiting for.",
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
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Minimal detector for the pattern matcher used by the enforcement layer
/// below (open-code-style globs: `*` = zero-or-more of any char, `?` = one
/// char, everything else literal — verified against the permissions docs).
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

/// Evaluate a permission map's rules in order, last match wins (the
/// documented evaluation order), and return the governing action.
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

#[test]
fn core_permission_templates_preserve_trust_domain_semantics() {
    // The two non-negotiables from AGENTS.md, checked at the data level so a
    // template edit cannot silently weaken them.
    let store = store();
    let core: Templates = store.core_templates();

    let operator = AgentTemplate::load(&core.agents, "operator").unwrap();
    assert_eq!(operator.mode, "primary", "operator must stay a primary agent");
    let op_perm = corpus_core::PermissionTemplate::load(&core.permissions, "operator").unwrap();
    let op_map: serde_yaml::Mapping =
        serde_yaml::from_str(&op_perm.permission).expect("operator permission parses");
    for key in ["bash", "edit", "write", "read", "glob", "grep", "list", "external_directory", "webfetch", "websearch", "task"] {
        assert_eq!(
            op_map.get(serde_yaml::Value::String(key.into())).and_then(|v| v.as_str()),
            Some("deny"),
            "operator must deny {key}"
        );
    }

    let researcher = AgentTemplate::load(&core.agents, "researcher").unwrap();
    assert_eq!(researcher.mode, "primary");
    let res_perm = corpus_core::PermissionTemplate::load(&core.permissions, "researcher").unwrap();
    let res_map: serde_yaml::Mapping =
        serde_yaml::from_str(&res_perm.permission).expect("researcher permission parses");
    let get = |k: &str| {
        res_map
            .get(serde_yaml::Value::String(k.into()))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    };
    // executes nothing
    for k in ["bash", "task", "corpus_sandbox_exec", "corpus_faucet", "corpus_wallet_fund", "corpus_oracle_run", "corpus_finding_write", "corpus_attack_save"] {
        assert_eq!(get(k), "deny", "researcher must deny {k}");
    }
    // store write access stays team-scoped, benchmark reads stay denied
    let write = res_map.get(serde_yaml::Value::String("write".into())).unwrap();
    let write_map = write.as_mapping().expect("researcher write is a rule map");
    assert_eq!(write_map.get(serde_yaml::Value::String("*".into())).and_then(|v| v.as_str()), Some("deny"));
    // The promotion-gate bypass is closed: the researcher may write the TEAM
    // corpus but never the project-global corpus, and may not promote.
    let team_corpus = "store/projects/p/teams/t/corpus/findings/x.md";
    let project_corpus = "store/projects/p/corpus/findings/x.md";
    let team_pattern = "store/projects/*/teams/*/corpus/**";
    assert!(matches(team_pattern, team_corpus), "team corpus matches the allow pattern");
    assert!(!matches(team_pattern, project_corpus), "project corpus cannot match the team pattern");
    assert_eq!(
        permission_for(write_map, team_corpus).as_deref(),
        Some("allow"),
        "team-scoped writes are allowed"
    );
    assert_eq!(
        permission_for(write_map, project_corpus).as_deref(),
        Some("deny"),
        "project-global corpus writes resolve to deny for the researcher"
    );
    assert_eq!(
        permission_for(write_map, "store/hypotheses/older-embedded-path.md").as_deref(),
        Some("deny"),
        "legacy flat-store write paths also resolve to deny"
    );
    assert_eq!(get("corpus_promote"), "deny", "researcher cannot promote (gate stays operator-only)");
    let read = res_map.get(serde_yaml::Value::String("read".into())).unwrap();
    assert_eq!(read.get(serde_yaml::Value::String("*".into())).and_then(|v| v.as_str()), Some("allow"));
    assert_eq!(read.get(serde_yaml::Value::String("benchmarks/**".into())).and_then(|v| v.as_str()), Some("deny"));
    let edit = res_map.get(serde_yaml::Value::String("edit".into())).unwrap();
    let edit_map = edit.as_mapping().expect("researcher edit is a rule map");
    assert_eq!(
        permission_for(edit_map, project_corpus).as_deref(),
        Some("deny"),
        "edit (covers write/patch) also denies the project corpus"
    );
    // research-zone openings
    assert_eq!(get("webfetch"), "allow");
    assert_eq!(get("websearch"), "allow");
    assert_eq!(get("corpus_target_info"), "allow");
    assert_eq!(get("corpus_technique_save"), "allow");
}