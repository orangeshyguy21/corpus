//! Corpus curation: the guard that decides which paths a caller may
//! delete, move or read inside a project's corpus.
//!
//! The entries are markdown on disk, and the one place an agent is allowed
//! to write with its own file tools — so "it is a relative path with no
//! `..` in it" is not sufficient. A link planted inside the corpus is a
//! legal relative path that resolves anywhere; only canonicalization
//! catches it, which is why the guard ends there rather than starting
//! there.

use std::fs;
use std::path::PathBuf;

use corpus_store::{EntryAccess, Store};

fn rig(tag: &str) -> (Store, PathBuf) {
    let world = std::env::temp_dir().join(format!("corpus-curation-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    let store = Store::new(world.join("store"));
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let corpus = store.project_corpus_dir("p");
    fs::write(corpus.join("findings/f1.md"), "finding\n").unwrap();
    fs::write(corpus.join("hypotheses/h1.md"), "hypothesis\n").unwrap();
    fs::create_dir_all(corpus.join("probes/replay")).unwrap();
    fs::write(corpus.join("probes/replay/probe.md"), "probe\n").unwrap();
    fs::write(corpus.join("probes/replay/run.sh"), "#!/bin/sh\n").unwrap();
    fs::write(corpus.join("runs/1786-op.raw"), "transcript\n").unwrap();
    (store, world)
}

#[test]
fn a_relative_entry_inside_the_corpus_resolves() {
    let (store, world) = rig("ok");
    let path = store
        .resolve_corpus_entry("p", "findings/f1.md", EntryAccess::Mutate)
        .unwrap();
    assert!(path.starts_with(store.project_corpus_dir("p").canonicalize().unwrap()));
    assert!(path.ends_with("findings/f1.md"));
    let _ = fs::remove_dir_all(&world);
}

#[test]
fn legacy_attacks_are_readable_and_deletable_but_not_writable() {
    let (store, world) = rig("legacy-category");
    let corpus = store.project_corpus_dir("p");
    fs::create_dir_all(corpus.join("attacks/legacy")).unwrap();
    fs::write(corpus.join("attacks/legacy/attack.md"), "legacy\n").unwrap();

    store
        .resolve_corpus_entry("p", "attacks/legacy/attack.md", EntryAccess::Read)
        .expect("legacy artifacts remain readable during migration");
    assert!(store
        .resolve_corpus_entry("p", "attacks/new/attack.md", EntryAccess::Destination)
        .is_err());
    store
        .delete_corpus_entry("p", "attacks/legacy", true)
        .expect("legacy artifacts remain deletable for cleanup");
    assert!(!corpus.join("attacks/legacy").exists());
    let _ = fs::remove_dir_all(&world);
}

/// Absolutes and traversal, refused textually before the filesystem is
/// touched at all.
#[test]
fn traversal_and_absolutes_are_refused() {
    let (store, world) = rig("traversal");
    for bad in [
        "/etc/passwd",
        "../../../etc/passwd",
        "findings/../../agents/keeper/agent.yaml",
        "findings/../../../store",
        "",
        "   ",
    ] {
        assert!(
            store
                .resolve_corpus_entry("p", bad, EntryAccess::Mutate)
                .is_err(),
            "must refuse {bad:?}"
        );
    }
    let _ = fs::remove_dir_all(&world);
}

/// The case textual guards miss: a symlink planted inside the corpus is a
/// perfectly ordinary relative path, and it resolves wherever it likes.
#[test]
fn a_symlink_planted_inside_the_corpus_does_not_widen_it() {
    let (store, world) = rig("symlink");
    let corpus = store.project_corpus_dir("p");
    // The agent tree is the prize: it holds the sidecars the role gate
    // trusts. A link to it from inside the corpus would make it writable
    // through a path that passes every textual check — so the target here
    // is a REAL file, to prove the guard refuses on where the path lands
    // rather than on the file happening not to exist.
    store
        .create_agent_with_role("p", "keeper", corpus_store::AgentRole::Researcher)
        .unwrap();
    let sidecar = store.project_agent_dir("p", "keeper").join("agent.yaml");
    assert!(sidecar.is_file(), "the escape target exists");
    std::os::unix::fs::symlink(
        store.project_agents_dir("p"),
        corpus.join("findings/escape"),
    )
    .unwrap();
    let error = store
        .resolve_corpus_entry(
            "p",
            "findings/escape/keeper/agent.yaml",
            EntryAccess::Mutate,
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("outside"), "{error}");
    // And the delete path refuses it too, not just the resolver.
    assert!(store
        .delete_corpus_entry("p", "findings/escape/keeper/agent.yaml", false)
        .is_err());
    assert!(sidecar.is_file(), "the sidecar survives");

    // The link itself is inside the corpus, so removing it is allowed —
    // the guard resolves the LINK, not its target, for a delete.
    std::os::unix::fs::symlink("/etc", corpus.join("findings/out")).unwrap();
    assert!(store
        .resolve_corpus_entry("p", "findings/out/passwd", EntryAccess::Mutate)
        .is_err());
    let _ = fs::remove_dir_all(&world);
}

/// `runs/` is refused outright: technique cards cite those transcripts by
/// name, so deleting one retro-orphans every card that cites it.
#[test]
fn run_transcripts_are_not_curatable() {
    let (store, world) = rig("runs");
    for path in ["runs/1786-op.raw", "runs", "runs/nested/x.json"] {
        let error = store
            .resolve_corpus_entry("p", path, EntryAccess::Mutate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("runs"), "{path}: {error}");
    }
    // But READING one is fine: an operator's chat lists and reads
    // transcripts, and a curator may want its own.
    store
        .resolve_corpus_entry("p", "runs/1786-op.raw", EntryAccess::Read)
        .expect("a transcript is readable");
    assert!(store
        .delete_corpus_entry("p", "runs/1786-op.raw", false)
        .is_err());
    assert!(store
        .move_corpus_entry("p", "runs/1786-op.raw", "findings/stolen.raw", false)
        .is_err());
    assert!(
        store
            .project_corpus_dir("p")
            .join("runs/1786-op.raw")
            .is_file(),
        "the transcript survives every attempt"
    );
    let _ = fs::remove_dir_all(&world);
}

/// A bare category is the whole shelf, not an entry — removing one is a
/// corpus wipe wearing a different name.
#[test]
fn a_bare_category_is_not_an_entry() {
    let (store, world) = rig("category");
    for path in ["findings", "probes", "hypotheses"] {
        let error = store
            .resolve_corpus_entry("p", path, EntryAccess::Mutate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("whole category"), "{path}: {error}");
    }
    // And a path naming no category at all.
    assert!(store
        .resolve_corpus_entry("p", "notes/x.md", EntryAccess::Mutate)
        .is_err());
    let _ = fs::remove_dir_all(&world);
}

/// Probes are directories, so a delete that silently recursed would take a
/// whole artifact on a one-word slip.
#[test]
fn deleting_a_directory_needs_saying_so() {
    let (store, world) = rig("recursive");
    let error = store
        .delete_corpus_entry("p", "probes/replay", false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("recursive"), "{error}");
    assert!(store.project_corpus_dir("p").join("probes/replay").is_dir());

    let freed = store
        .delete_corpus_entry("p", "probes/replay", true)
        .unwrap();
    assert!(freed > 0, "reports the bytes it freed");
    assert!(!store.project_corpus_dir("p").join("probes/replay").exists());

    // A file needs no flag.
    store
        .delete_corpus_entry("p", "findings/f1.md", false)
        .unwrap();
    assert!(!store
        .project_corpus_dir("p")
        .join("findings/f1.md")
        .exists());
    let _ = fs::remove_dir_all(&world);
}

#[test]
fn moving_reorganises_within_the_corpus_and_refuses_to_clobber() {
    let (store, world) = rig("move");
    let corpus = store.project_corpus_dir("p");

    // Rename inside a category.
    store
        .move_corpus_entry("p", "findings/f1.md", "findings/renamed.md", false)
        .unwrap();
    assert!(corpus.join("findings/renamed.md").is_file());
    assert!(!corpus.join("findings/f1.md").exists());

    // Move across categories, into a subdirectory that does not exist yet.
    store
        .move_corpus_entry("p", "findings/renamed.md", "hypotheses/2026/lead.md", false)
        .unwrap();
    assert!(corpus.join("hypotheses/2026/lead.md").is_file());

    // Clobbering is refused unless asked for.
    let error = store
        .move_corpus_entry("p", "hypotheses/2026/lead.md", "hypotheses/h1.md", false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exists"), "{error}");
    store
        .move_corpus_entry("p", "hypotheses/2026/lead.md", "hypotheses/h1.md", true)
        .unwrap();
    assert_eq!(
        fs::read_to_string(corpus.join("hypotheses/h1.md")).unwrap(),
        "finding\n",
        "overwrite replaces the destination"
    );

    // Neither end may leave the corpus.
    assert!(store
        .move_corpus_entry("p", "hypotheses/h1.md", "../../escaped.md", false)
        .is_err());
    assert!(store
        .move_corpus_entry("p", "hypotheses/h1.md", "runs/x.md", false)
        .is_err());
    let _ = fs::remove_dir_all(&world);
}

/// One project's guard cannot resolve into another's corpus, even though
/// they are siblings on disk.
#[test]
fn the_guard_is_per_project() {
    let (store, world) = rig("scoped");
    store.create_project("other", "O", "cdk-regtest").unwrap();
    fs::write(
        store.project_corpus_dir("other").join("findings/secret.md"),
        "theirs\n",
    )
    .unwrap();
    let resolved = store
        .resolve_corpus_entry("p", "findings/f1.md", EntryAccess::Mutate)
        .unwrap();
    assert!(
        !resolved.starts_with(store.project_corpus_dir("other")),
        "p's guard resolves inside p"
    );
    assert!(store
        .resolve_corpus_entry(
            "p",
            "../../other/corpus/findings/secret.md",
            EntryAccess::Mutate
        )
        .is_err());
    let _ = fs::remove_dir_all(&world);
}
