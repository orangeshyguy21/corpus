//! What a role RENDERS to.
//!
//! The predecessor of this file compared renders byte-for-byte against
//! `.opencode/agent/*.md` checked into the repo root. That coupling is the
//! reason eight stale, project-bound agent files sat in a directory
//! opencode discovers: the fixture WAS the artifact. Fixtures now live
//! under `tests/fixtures/`, which nothing discovers, and the assertions
//! below are mostly semantic — one byte-equality canary, everything else
//! about meaning.
//!
//! Refresh the fixtures with `CORPUS_BLESS=1 cargo test -p corpus-core
//! --test roles` and read the diff before committing it.

use std::fs;
use std::path::{Path, PathBuf};

use corpus_core::{AgentRole, Store};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A store in its own world (run dirs are siblings of the store).
fn tmp_store(tag: &str) -> Store {
    let world = std::env::temp_dir().join(format!("corpus-roles-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    Store::new(world.join("store"))
}

/// Create one agent per role in a project and render the set.
fn render_role(store: &Store, project: &str, role: AgentRole) -> String {
    let slug = role.as_str();
    store.create_agent_with_role(project, slug, role).unwrap();
    store.render_project_agents(project, &[]).unwrap();
    fs::read_to_string(store.opencode_agent_dir(project).join(format!("{slug}.md"))).unwrap()
}

fn frontmatter(agent_raw: &str) -> serde_yaml::Mapping {
    let (fm, _body) = corpus_core::frontmatter::split(agent_raw).expect("parse frontmatter");
    fm.expect("agent file has frontmatter")
}

fn perm(agent_raw: &str) -> serde_yaml::Mapping {
    frontmatter(agent_raw)["permission"]
        .as_mapping()
        .cloned()
        .expect("permission is a mapping")
}

fn action(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    map.get(serde_yaml::Value::String(key.into()))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Glob matcher mirroring the enforcement layer's.
fn matches(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.split_first(), t.split_first()) {
            (None, None) => true,
            (Some((b'*', rest)), _) => go(rest, t) || (!t.is_empty() && go(p, &t[1..])),
            (Some((b'?', rest)), Some((_, t_rest))) => go(rest, t_rest),
            (Some((pat, rest)), Some((txt, t_rest))) if pat == txt => go(rest, t_rest),
            _ => false,
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// Evaluate a rule map the way opencode does: last match wins, over the
/// keys in the order the rendered file lists them.
fn permission_for(map: &serde_yaml::Mapping, path: &str) -> Option<String> {
    let mut out = None;
    for (k, v) in map {
        let (Some(pattern), Some(act)) = (k.as_str(), v.as_str()) else {
            continue;
        };
        if matches(pattern, path) {
            out = Some(act.to_string());
        }
    }
    out
}

/// The canary: one role's render, byte for byte. Catches accidental
/// reordering, whitespace drift and frontmatter churn that the semantic
/// assertions below would sail past.
#[test]
fn a_researcher_renders_to_its_fixture() {
    let store = tmp_store("fixture");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let rendered = render_role(&store, "p", AgentRole::Researcher);
    // The absolute-boundary rules name the store and its parent, so
    // normalize both out — the fixture must not depend on where the test
    // ran. Longest first: the store path contains the data path.
    let rendered = rendered.replace(&store.root().display().to_string(), "<STORE>");
    let rendered = rendered.replace(
        &store.root().parent().unwrap().display().to_string(),
        "<DATA>",
    );
    let path = fixture_dir().join("role-researcher.md");

    if std::env::var("CORPUS_BLESS").as_deref() == Ok("1") {
        fs::create_dir_all(fixture_dir()).unwrap();
        fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} — regenerate with CORPUS_BLESS=1 cargo test -p corpus-core --test roles",
            path.display()
        )
    });
    assert_eq!(
        rendered,
        expected,
        "the researcher render drifted from {}. If the new output is right, \
         re-bless with CORPUS_BLESS=1 and read the diff.",
        path.display()
    );
}

/// The trust domains, asserted on the RENDERED artifact rather than on a
/// stored permission block — the render is what opencode obeys, and the
/// role is now the only source of truth about capability.
#[test]
fn roles_bind_their_trust_domains() {
    let store = tmp_store("domains");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "researcher", AgentRole::Researcher)
        .unwrap();
    store
        .create_agent_with_role("p", "tester", AgentRole::Tester)
        .unwrap();
    store.render_project_agents("p", &[]).unwrap();
    let read_render = |slug: &str| {
        fs::read_to_string(store.opencode_agent_dir("p").join(format!("{slug}.md"))).unwrap()
    };

    // Researcher: reads and curates, executes nothing, keeps the internet.
    let res = perm(&read_render("researcher"));
    for denied in [
        "corpus_sandbox_exec",
        "corpus_oracle_run",
        "corpus_faucet",
        "corpus_wallet_fund",
        "corpus_finding_write",
        "corpus_attack_save",
    ] {
        assert_eq!(action(&res, denied).as_deref(), Some("deny"), "{denied}");
    }
    assert_eq!(action(&res, "corpus_target_info").as_deref(), Some("allow"));
    assert_eq!(action(&res, "corpus_technique_save").as_deref(), Some("allow"));
    assert_eq!(action(&res, "webfetch").as_deref(), Some("allow"));
    assert_eq!(action(&res, "websearch").as_deref(), Some("allow"));
    // A host shell would let it forge the identity the server's role gate
    // trusts, so the ceiling would be a fiction.
    assert_eq!(action(&res, "bash").as_deref(), Some("deny"));
    // The run dir exposes one project; stepping outside it is refused too.
    assert_eq!(action(&res, "external_directory").as_deref(), Some("deny"));

    // Tester: acts in the sandbox, loses the open internet.
    let tester = perm(&read_render("tester"));
    for allowed in ["corpus_sandbox_exec", "corpus_oracle_run", "corpus_finding_write"] {
        assert_eq!(action(&tester, allowed).as_deref(), Some("allow"), "{allowed}");
    }
    assert_eq!(action(&tester, "webfetch").as_deref(), Some("deny"));
    assert_eq!(action(&tester, "websearch").as_deref(), Some("deny"));

    // Neither may delegate to anything, since neither declares a subagent.
    for map in [&res, &tester] {
        let task = map
            .get(serde_yaml::Value::String("task".into()))
            .and_then(|v| v.as_mapping())
            .expect("task is always written");
        assert_eq!(
            task.get(serde_yaml::Value::String("*".into())).and_then(|v| v.as_str()),
            Some("deny")
        );
    }
}

/// The contamination rule and the project boundary, evaluated as opencode
/// would evaluate them — by path, not by key presence.
#[test]
fn the_boundary_holds_by_evaluation() {
    let store = tmp_store("boundary");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let rendered = render_role(&store, "p", AgentRole::Researcher);
    let perms = perm(&rendered);
    let read = perms["read"].as_mapping().cloned().unwrap();
    let write = perms["write"].as_mapping().cloned().unwrap();
    let root = store.root().display().to_string();

    for (path, expected, why) in [
        ("benchmarks/CDK-BENCH-0001.yaml", "deny", "the answer key"),
        ("plugins/cdk-regtest/setup.sh", "deny", "harness internals"),
        ("store/projects/p/corpus/hypotheses/x.md", "allow", "own corpus"),
        ("store/projects/other/corpus/findings/x.md", "deny", "another project"),
        (
            &format!("{root}/projects/other/corpus/findings/x.md"),
            "deny",
            "another project, by absolute path",
        ),
    ] {
        assert_eq!(
            permission_for(&read, path).as_deref(),
            Some(expected),
            "read {why}: {path}"
        );
    }

    for (path, expected, why) in [
        ("store/projects/p/corpus/findings/x.md", "allow", "own corpus"),
        ("store/projects/p/agents/researcher/agent.yaml", "deny", "own sidecars"),
        ("store/projects/other/corpus/findings/x.md", "deny", "another project"),
        ("store/hypotheses/legacy-flat-path.md", "deny", "the legacy flat store"),
        // Transcripts are inside the corpus and still not writable: cards
        // cite them by name, the cost report counts them, and they are the
        // provenance an operator audits.
        ("store/projects/p/corpus/runs/1786-x.raw", "deny", "a run transcript"),
        (
            &format!("{root}/projects/p/corpus/runs/1786-x.raw"),
            "deny",
            "a run transcript, by absolute path",
        ),
    ] {
        assert_eq!(
            permission_for(&write, path).as_deref(),
            Some(expected),
            "write {why}: {path}"
        );
    }

    // Reading one is fine — an agent may want its own transcript.
    assert_eq!(
        permission_for(&read, "store/projects/p/corpus/runs/1786-x.raw").as_deref(),
        Some("allow"),
        "a transcript stays readable"
    );
}

/// Every role's rendered frontmatter must PARSE.
///
/// It did not: descriptions were interpolated raw, so the `super` role —
/// whose own description contains a colon — rendered a file whose
/// frontmatter is not valid YAML. opencode reads the permission block out
/// of that frontmatter, so the entire opencode-side half of the role gate
/// went missing for every agent rendered under it, silently. Nothing
/// asserted this because the only fixture was a researcher, whose
/// description happens to be colon-free.
#[test]
fn every_role_renders_parseable_frontmatter() {
    for role in AgentRole::ALL {
        let store = tmp_store(&format!("fm-{}", role.as_str()));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let rendered = render_role(&store, "p", role);
        let fm = frontmatter(&rendered);
        assert_eq!(
            fm["description"].as_str(),
            Some(role.default_description()),
            "{} description must survive the round trip intact",
            role.as_str()
        );
        assert!(
            fm["permission"].as_mapping().is_some(),
            "{}: the permission block must be reachable — it is the gate",
            role.as_str()
        );
    }
}

/// Every role produces a usable agent: a prompt, a description, and the
/// launch-bound scope footer that names its corpus.
#[test]
fn every_role_produces_a_bound_agent() {
    for role in AgentRole::ALL {
        let store = tmp_store(&format!("all-{}", role.as_str()));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let rendered = render_role(&store, "p", role);
        assert!(
            rendered.contains("## Corpus scope (bound at launch)"),
            "{role:?} render carries the scope footer"
        );
        assert!(
            rendered.contains("You are bound to project `p`"),
            "{role:?} render names its project"
        );
        assert!(
            !role.default_prompt().trim().is_empty(),
            "{role:?} has a starting prompt"
        );
        // The prompt must not hardcode a corpus path: agents get cloned
        // across projects, and the footer is what names the corpus.
        assert!(
            !role.default_prompt().contains("store/projects/"),
            "{role:?} prompt must not hardcode a project path"
        );
    }
}
