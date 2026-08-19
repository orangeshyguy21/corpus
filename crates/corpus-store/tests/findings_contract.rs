//! The finding projection contract, frozen before its production parser.
//!
//! These fixtures are intentionally heterogeneous: the UI hook is a tolerant
//! projection over agent-authored Markdown, not a mandatory document shape.
//! Chunk 1 extends this test target to run the core parser against the same
//! expected outcomes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use corpus_store::{
    query_findings, scan_findings_cached, FindingIndexCache, FindingQuery, FindingSeverity,
    FindingSort, FindingWarning, NewFinding, Store, FINDING_PREFIX_LIMIT,
};
use serde::Deserialize;

static NEXT_WORLD: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    version: u64,
    benchmark: BenchmarkRecipe,
    cases: Vec<Case>,
    newest_order: Vec<String>,
    severity_order: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkRecipe {
    copies_per_case: usize,
    minimum_cards: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    file: PathBuf,
    modified_time: u64,
    #[serde(default)]
    append_body_bytes: usize,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    title: String,
    title_source: String,
    severity: Option<String>,
    timestamp: u64,
    time_source: String,
    reference: String,
    reference_source: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    oracle_verified: Option<bool>,
    warnings: Vec<String>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/findings")
}

fn contract() -> Contract {
    let path = fixture_dir().join("contract.yaml");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_yaml::from_str(&raw).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
                found.push(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    found.sort();
    found
}

fn severity_rank(value: Option<&str>) -> u8 {
    match value {
        Some("critical") => 0,
        Some("high") => 1,
        Some("medium") => 2,
        Some("low") => 3,
        None => 4,
        Some(other) => panic!("contract contains unsupported severity {other:?}"),
    }
}

fn materialize(contract: &Contract, tag: &str) -> (Store, PathBuf) {
    let id = NEXT_WORLD.fetch_add(1, Ordering::Relaxed);
    let world =
        std::env::temp_dir().join(format!("corpus-findings-{tag}-{}-{id}", std::process::id()));
    let store = Store::new(world.join("store"));
    store
        .create_project("project", "Project", "cdk-regtest")
        .unwrap();
    let findings = store.project_corpus_dir("project").join("findings");
    for case in &contract.cases {
        let source = fixture_dir().join(&case.file);
        let destination = findings.join(&case.file);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let mut raw = fs::read(&source).unwrap();
        raw.resize(raw.len() + case.append_body_bytes, b'x');
        fs::write(&destination, raw).unwrap();
        let modified = std::fs::FileTimes::new()
            .set_modified(UNIX_EPOCH + Duration::from_secs(case.modified_time));
        fs::File::options()
            .write(true)
            .open(&destination)
            .unwrap()
            .set_times(modified)
            .unwrap();
    }
    (store, world)
}

fn case_for_card<'a>(contract: &'a Contract, path: &Path) -> &'a Case {
    let relative = path.strip_prefix("findings").unwrap();
    contract
        .cases
        .iter()
        .find(|case| case.file == relative)
        .unwrap_or_else(|| panic!("no contract case for {}", path.display()))
}

#[test]
fn finding_contract_is_complete_and_self_consistent() {
    let contract = contract();
    assert_eq!(contract.version, 1);
    assert!(
        contract.cases.len() * contract.benchmark.copies_per_case
            >= contract.benchmark.minimum_cards,
        "the P1.0 recipe must materialize a high-cardinality finding corpus"
    );

    let expected_ids = BTreeSet::from([
        "canonical-high",
        "critical",
        "h1-only",
        "invalid-severity",
        "invalid-yaml",
        "large-body",
        "nested-name-only",
        "no-frontmatter",
        "optional-fields-low",
    ]);
    let actual_ids: BTreeSet<&str> = contract.cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(
        actual_ids, expected_ids,
        "a named contract case was added or lost"
    );

    let declared_files: BTreeSet<PathBuf> = contract
        .cases
        .iter()
        .map(|case| case.file.clone())
        .collect();
    let actual_files: BTreeSet<PathBuf> = markdown_files(&fixture_dir()).into_iter().collect();
    assert_eq!(
        actual_files, declared_files,
        "every Markdown fixture needs expectations"
    );

    let allowed_warnings = BTreeSet::from([
        "invalid-frontmatter",
        "invalid-severity",
        "missing-frontmatter",
        "missing-severity",
    ]);
    let allowed_title_sources = BTreeSet::from(["title", "name", "h1", "filename"]);
    let allowed_time_sources = BTreeSet::from(["timestamp", "filename", "modified"]);
    let allowed_reference_sources = BTreeSet::from(["id", "path"]);
    let mut severity_coverage = BTreeSet::new();
    let mut saw_unrated = false;
    let mut saw_nested = false;
    let mut saw_generated_large_body = false;

    for case in &contract.cases {
        assert!(
            !case.file.is_absolute()
                && case.file.extension().and_then(|value| value.to_str()) == Some("md")
                && case
                    .file
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "{} must be a safe relative Markdown path",
            case.file.display()
        );
        saw_nested |= case.file.components().count() > 1;

        let path = fixture_dir().join(&case.file);
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.is_empty(), "{} is empty", case.file.display());
        assert!(allowed_title_sources.contains(case.expected.title_source.as_str()));
        assert!(allowed_time_sources.contains(case.expected.time_source.as_str()));
        assert!(allowed_reference_sources.contains(case.expected.reference_source.as_str()));
        assert!(case
            .expected
            .warnings
            .iter()
            .all(|warning| { allowed_warnings.contains(warning.as_str()) }));

        match case.expected.severity.as_deref() {
            Some(severity) => {
                severity_rank(Some(severity));
                severity_coverage.insert(severity);
                assert!(
                    raw.contains(&format!("severity: {severity}")),
                    "{} does not declare its expected severity",
                    case.file.display()
                );
            }
            None => saw_unrated = true,
        }

        match case.expected.title_source.as_str() {
            "title" => assert!(raw.contains(&format!("title: {}", case.expected.title))),
            "name" => assert!(raw.contains(&format!("name: {}", case.expected.title))),
            "h1" => assert!(raw
                .lines()
                .any(|line| line == format!("# {}", case.expected.title))),
            "filename" => assert_eq!(
                case.file.file_stem().and_then(|value| value.to_str()),
                Some(case.expected.title.as_str())
            ),
            _ => unreachable!(),
        }

        match case.expected.time_source.as_str() {
            "timestamp" => assert!(
                raw.contains(&format!("timestamp: {}", case.expected.timestamp)),
                "{} does not declare its expected timestamp",
                case.file.display()
            ),
            "filename" => {
                let prefix = case
                    .file
                    .file_name()
                    .and_then(|value| value.to_str())
                    .and_then(|name| name.split_once('-'))
                    .map(|(prefix, _)| prefix);
                assert_eq!(prefix, Some(case.expected.timestamp.to_string().as_str()));
            }
            "modified" => assert_eq!(case.expected.timestamp, case.modified_time),
            _ => unreachable!(),
        }

        if case.expected.reference_source == "id" {
            assert!(raw.contains(&format!("id: {}", case.expected.reference)));
        } else {
            assert_eq!(
                case.expected.reference,
                format!("findings/{}", case.file.display())
            );
        }

        if let Some(status) = &case.expected.status {
            assert!(raw.contains(&format!("status: {status}")));
        }
        if let Some(verified) = case.expected.oracle_verified {
            assert!(raw.contains(&format!("oracle_verified: {verified}")));
        }

        if case.append_body_bytes > 0 {
            saw_generated_large_body = true;
            assert!(
                raw.len() < 4 * 1024,
                "large-body seed must keep a small prefix"
            );
            assert!(
                raw.len() + case.append_body_bytes >= 256 * 1024,
                "materialized large-body case must cross the bounded-read fixture size"
            );
        }
    }

    assert_eq!(
        severity_coverage,
        BTreeSet::from(["critical", "high", "low", "medium"])
    );
    assert!(saw_unrated, "legacy/unrated behavior needs a fixture");
    assert!(saw_nested, "recursive organization needs a fixture");
    assert!(
        saw_generated_large_body,
        "bounded prefix reads need a large-body recipe"
    );

    let by_id: BTreeMap<&str, &Case> = contract
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect();

    let mut newest: Vec<&Case> = contract.cases.iter().collect();
    newest.sort_by(|left, right| {
        right
            .expected
            .timestamp
            .cmp(&left.expected.timestamp)
            .then_with(|| left.file.cmp(&right.file))
    });
    assert_eq!(
        newest
            .iter()
            .map(|case| case.id.as_str())
            .collect::<Vec<_>>(),
        contract
            .newest_order
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );

    let mut by_severity: Vec<&Case> = contract.cases.iter().collect();
    by_severity.sort_by(|left, right| {
        severity_rank(left.expected.severity.as_deref())
            .cmp(&severity_rank(right.expected.severity.as_deref()))
            .then_with(|| right.expected.timestamp.cmp(&left.expected.timestamp))
            .then_with(|| left.file.cmp(&right.file))
    });
    assert_eq!(
        by_severity
            .iter()
            .map(|case| case.id.as_str())
            .collect::<Vec<_>>(),
        contract
            .severity_order
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );

    for id in contract.newest_order.iter().chain(&contract.severity_order) {
        assert!(
            by_id.contains_key(id.as_str()),
            "ordering names unknown case {id}"
        );
    }
}

#[test]
fn production_projection_matches_the_contract() {
    let contract = contract();
    let (store, world) = materialize(&contract, "projection");
    let mut cache = FindingIndexCache::default();
    let missing = corpus_store::finding_cards(&store, "missing")
        .unwrap_err()
        .to_string();
    assert!(missing.contains("project not found"), "{missing}");
    let scan = scan_findings_cached(&store, "project", &mut cache, || false).unwrap();

    assert_eq!(scan.cards.len(), contract.cases.len());
    assert_eq!(scan.stats.parsed_files, contract.cases.len());
    assert_eq!(scan.stats.cached_files, 0);
    assert!(
        scan.stats.bytes_read <= (contract.cases.len() * (FINDING_PREFIX_LIMIT + 1)) as u64,
        "the index read beyond its per-file prefix bound: {:?}",
        scan.stats
    );

    for card in &scan.cards {
        let case = case_for_card(&contract, &card.path);
        let expected = &case.expected;
        assert_eq!(card.title, expected.title, "{}", case.id);
        assert_eq!(
            card.title_source.as_str(),
            expected.title_source,
            "{}",
            case.id
        );
        assert_eq!(
            card.severity.map(|severity| severity.as_str()),
            expected.severity.as_deref(),
            "{}",
            case.id
        );
        assert_eq!(card.timestamp, Some(expected.timestamp), "{}", case.id);
        assert_eq!(
            card.time_source.map(|source| source.as_str()),
            Some(expected.time_source.as_str()),
            "{}",
            case.id
        );
        assert_eq!(card.reference, expected.reference, "{}", case.id);
        assert_eq!(
            card.reference_source.as_str(),
            expected.reference_source,
            "{}",
            case.id
        );
        assert_eq!(card.status, expected.status, "{}", case.id);
        assert_eq!(
            card.oracle_verified, expected.oracle_verified,
            "{}",
            case.id
        );
        assert_eq!(
            card.warnings
                .iter()
                .map(|warning| warning.as_str())
                .collect::<Vec<_>>(),
            expected
                .warnings
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "{}",
            case.id
        );
    }

    assert_eq!(
        scan.cards
            .iter()
            .map(|card| case_for_card(&contract, &card.path).id.as_str())
            .collect::<Vec<_>>(),
        contract
            .newest_order
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let severity_order = query_findings(
        &scan.cards,
        &FindingQuery {
            sort: FindingSort::Severity,
            ..FindingQuery::default()
        },
    );
    assert_eq!(
        severity_order
            .iter()
            .map(|card| case_for_card(&contract, &card.path).id.as_str())
            .collect::<Vec<_>>(),
        contract
            .severity_order
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(world);
}

#[test]
fn cache_reparses_only_changed_files_and_query_is_in_memory() {
    let contract = contract();
    let (store, world) = materialize(&contract, "cache");
    let mut cache = FindingIndexCache::default();
    let first = scan_findings_cached(&store, "project", &mut cache, || false).unwrap();
    assert_eq!(first.stats.parsed_files, contract.cases.len());

    let second = scan_findings_cached(&store, "project", &mut cache, || false).unwrap();
    assert_eq!(second.stats.parsed_files, 0);
    assert_eq!(second.stats.cached_files, contract.cases.len());
    assert_eq!(second.stats.bytes_read, 0);

    let changed = store
        .project_corpus_dir("project")
        .join("findings/1787091224-unauth-upload.md");
    let mut raw = fs::read(&changed).unwrap();
    raw.extend_from_slice(b"\nnew evidence\n");
    fs::write(&changed, raw).unwrap();
    let third = scan_findings_cached(&store, "project", &mut cache, || false).unwrap();
    assert_eq!(third.stats.parsed_files, 1);
    assert_eq!(third.stats.cached_files, contract.cases.len() - 1);

    let removed = store
        .project_corpus_dir("project")
        .join("findings/plain-note.md");
    fs::remove_file(&removed).unwrap();
    let fourth = scan_findings_cached(&store, "project", &mut cache, || false).unwrap();
    assert_eq!(fourth.cards.len(), contract.cases.len() - 1);
    assert_eq!(fourth.stats.parsed_files, 0);
    assert_eq!(fourth.stats.cached_files, contract.cases.len() - 1);
    assert!(fourth
        .cards
        .iter()
        .all(|card| card.path != Path::new("findings/plain-note.md")));

    let query = FindingQuery {
        severities: BTreeSet::from([FindingSeverity::High]),
        include_unrated: false,
        text: Some("large".into()),
        sort: FindingSort::Severity,
        limit: Some(1),
    };
    let result = query_findings(&fourth.cards, &query);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].title, "Large narrative finding");

    let _ = fs::remove_dir_all(world);
}

#[test]
fn cancellation_is_transactional_and_symlinks_are_not_followed() {
    let contract = contract();
    let (store, world) = materialize(&contract, "cancel");
    let mut cache = FindingIndexCache::default();
    let mut checks = 0;
    let error = scan_findings_cached(&store, "project", &mut cache, || {
        checks += 1;
        checks > 4
    })
    .unwrap_err();
    assert!(error.to_string().contains("finding scan cancelled"));

    let after = scan_findings_cached(&store, "project", &mut cache, || false).unwrap();
    assert_eq!(
        after.stats.parsed_files,
        contract.cases.len(),
        "a cancelled scan must not publish a partial cache"
    );

    #[cfg(unix)]
    {
        let outside = world.join("outside.md");
        fs::write(&outside, "# outside\n").unwrap();
        let link = store
            .project_corpus_dir("project")
            .join("findings/linked.md");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let scan = scan_findings_cached(&store, "project", &mut cache, || false).unwrap();
        assert!(
            scan.cards
                .iter()
                .all(|card| card.path != Path::new("findings/linked.md")),
            "discovery must not follow a planted symlink"
        );
    }

    assert!(
        after
            .cards
            .iter()
            .find(|card| card.title == "Large narrative finding")
            .is_some_and(|card| !card.warnings.contains(&FindingWarning::PrefixLimit)),
        "a large body with complete prefix metadata is not malformed"
    );
    let _ = fs::remove_dir_all(world);
}

fn draft(title: &str, path: Option<&str>) -> NewFinding {
    NewFinding {
        title: title.to_string(),
        severity: FindingSeverity::Critical,
        detail: "agent-defined detail".to_string(),
        timestamp: 1_787_091_300,
        oracle_verified: true,
        oracle_output: "fixture-invariant violated".to_string(),
        path: path.map(str::to_string),
        metadata: BTreeMap::new(),
        run_log: Some("1787091000-operator.raw".to_string()),
        actor: Some("tester:operator".to_string()),
        source_pins: None,
    }
}

#[test]
fn writer_serializes_reserved_and_extension_metadata_safely() {
    let contract = contract();
    let (store, world) = materialize(&contract, "writer-metadata");
    let mut finding = draft(
        "Line one\nseverity: low",
        Some("campaigns/august/report.md"),
    );
    finding
        .metadata
        .insert("id".into(), serde_json::json!("CDK-REG-900"));
    finding
        .metadata
        .insert("component".into(), serde_json::json!("mint: api"));
    finding
        .metadata
        .insert("cwes".into(), serde_json::json!(["CWE-20", "CWE-284"]));
    finding.source_pins = Some(serde_json::Map::from_iter([(
        "cdk".to_string(),
        serde_json::json!("0123456789abcdef"),
    )]));

    let written = store.write_finding("project", &finding).unwrap();
    assert_eq!(
        written.path,
        Path::new("findings/campaigns/august/report.md")
    );
    assert_eq!(written.reference, "CDK-REG-900");
    let raw = fs::read_to_string(store.project_corpus_dir("project").join(&written.path)).unwrap();
    let (frontmatter, body) = corpus_store::frontmatter::split(&raw).unwrap();
    let frontmatter = frontmatter.unwrap();
    assert_eq!(
        corpus_store::frontmatter::get_str(&frontmatter, "title").as_deref(),
        Some("Line one\nseverity: low")
    );
    assert_eq!(
        corpus_store::frontmatter::get_str(&frontmatter, "severity").as_deref(),
        Some("critical")
    );
    assert_eq!(
        corpus_store::frontmatter::get_str(&frontmatter, "sensitivity").as_deref(),
        Some("embargoed")
    );
    assert_eq!(
        corpus_store::frontmatter::get_str(&frontmatter, "actor").as_deref(),
        Some("tester:operator")
    );
    assert!(body.contains("agent-defined detail"));
    assert!(body.contains("    fixture-invariant violated"));

    finding
        .metadata
        .insert("severity".into(), serde_json::json!("low"));
    let error = store
        .write_finding("project", &finding)
        .unwrap_err()
        .to_string();
    assert!(error.contains("reserved"), "{error}");
    let _ = fs::remove_dir_all(world);
}

#[test]
fn writer_is_collision_safe_and_confines_agent_chosen_paths() {
    let contract = contract();
    let (store, world) = materialize(&contract, "writer-paths");
    let generated = draft("Same finding", Some("campaigns/retest"));
    let first = store.write_finding("project", &generated).unwrap();
    let second = store.write_finding("project", &generated).unwrap();
    assert_eq!(
        first.path,
        Path::new("findings/campaigns/retest/1787091300-same-finding.md")
    );
    assert_eq!(
        second.path,
        Path::new("findings/campaigns/retest/1787091300-same-finding-1.md")
    );
    let unicode = store
        .write_finding("project", &draft("Δ proof reuse", Some("campaigns/v1.0/")))
        .unwrap();
    assert_eq!(
        unicode.path,
        Path::new("findings/campaigns/v1.0/1787091300-proof-reuse.md")
    );
    let unicode_only = store.write_finding("project", &draft("Δ", None)).unwrap();
    assert_eq!(
        unicode_only.path,
        Path::new("findings/1787091300-finding.md")
    );

    let exact = draft("Exact finding", Some("custom/exact.md"));
    store.write_finding("project", &exact).unwrap();
    let before = fs::read(
        store
            .project_corpus_dir("project")
            .join("findings/custom/exact.md"),
    )
    .unwrap();
    assert_eq!(
        corpus_store::read_finding(&store, "project", "findings/custom/exact.md")
            .unwrap()
            .as_bytes(),
        before
    );
    let error = corpus_store::read_finding(&store, "project", "techniques/not-a-finding.md")
        .unwrap_err()
        .to_string();
    assert!(error.contains("beginning findings/"), "{error}");
    let error = store
        .write_finding("project", &exact)
        .unwrap_err()
        .to_string();
    assert!(error.contains("never overwrites"), "{error}");
    assert_eq!(
        fs::read(
            store
                .project_corpus_dir("project")
                .join("findings/custom/exact.md")
        )
        .unwrap(),
        before
    );

    for bad in [
        "../runs/stolen.md",
        "/tmp/out.md",
        "report.txt",
        "a\\..\\out.md",
    ] {
        let error = store
            .write_finding("project", &draft("Bad path", Some(bad)))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("path") || error.contains("component") || error.contains("extension"),
            "{bad}: {error}"
        );
    }

    #[cfg(unix)]
    {
        let outside = world.join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(
            &outside,
            store.project_corpus_dir("project").join("findings/escape"),
        )
        .unwrap();
        let error = store
            .write_finding("project", &draft("Linked path", Some("escape/out.md")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside"), "{error}");
        assert!(!outside.join("out.md").exists());
    }
    let _ = fs::remove_dir_all(world);
}
