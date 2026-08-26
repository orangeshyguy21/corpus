//! Tolerant finding severity, discovery, and canonical persistence.
//!
//! Markdown remains the artifact. This module projects just enough metadata
//! for coherent lists and filters; it never prescribes a body template or
//! rewrites an entry while reading it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Take, Write};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::corpus_entries::EntryAccess;
use crate::frontmatter;
use crate::store::slugify;
use crate::{Error, Result, Sensitivity, Store};

/// Maximum bytes read from one finding during index projection. Full bodies
/// are detail-view material and use a separate, explicitly bounded read path.
pub const FINDING_PREFIX_LIMIT: usize = 64 * 1024;

/// Frontmatter keys established by Corpus rather than extension metadata.
pub const FINDING_RESERVED_KEYS: [&str; 10] = [
    "title",
    "severity",
    "timestamp",
    "sensitivity",
    "oracle_verified",
    "run_log",
    "actor",
    "agent",
    "mission",
    "source_pins",
];

/// The shared severity vocabulary. Declaration order is risk order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Critical,
    High,
    Medium,
    Low,
}

impl FindingSeverity {
    pub const ALL: [Self; 4] = [Self::Critical, Self::High, Self::Medium, Self::Low];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl fmt::Display for FindingSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for FindingSeverity {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "critical" => Ok(Self::Critical),
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            _ => Err(format!(
                "invalid finding severity {value:?}; expected critical, high, medium, or low"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingTitleSource {
    Title,
    Name,
    Heading,
    FileName,
}

impl FindingTitleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Name => "name",
            Self::Heading => "h1",
            Self::FileName => "filename",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingTimeSource {
    Timestamp,
    FileName,
    Modified,
}

impl FindingTimeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timestamp => "timestamp",
            Self::FileName => "filename",
            Self::Modified => "modified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingReferenceSource {
    Id,
    Path,
}

impl FindingReferenceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Path => "path",
        }
    }
}

/// Metadata defects are card data, not reasons to hide an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingWarning {
    MissingFrontmatter,
    InvalidFrontmatter,
    MissingSeverity,
    InvalidSeverity,
    InvalidSensitivity,
    PrefixLimit,
    InvalidUtf8,
    Unreadable,
}

impl FindingWarning {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingFrontmatter => "missing-frontmatter",
            Self::InvalidFrontmatter => "invalid-frontmatter",
            Self::MissingSeverity => "missing-severity",
            Self::InvalidSeverity => "invalid-severity",
            Self::InvalidSensitivity => "invalid-sensitivity",
            Self::PrefixLimit => "prefix-limit",
            Self::InvalidUtf8 => "invalid-utf8",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Render/search projection of one Markdown artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingCard {
    /// Relative to the project corpus, always beginning `findings/`.
    pub path: PathBuf,
    pub title: String,
    pub title_source: FindingTitleSource,
    pub severity: Option<FindingSeverity>,
    pub timestamp: Option<u64>,
    pub time_source: Option<FindingTimeSource>,
    pub reference: String,
    pub reference_source: FindingReferenceSource,
    pub status: Option<String>,
    pub oracle_verified: Option<bool>,
    pub sensitivity: Option<Sensitivity>,
    pub warnings: Vec<FindingWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindingSort {
    #[default]
    Newest,
    Severity,
}

/// Pure in-memory query over prepared cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingQuery {
    /// Empty means every rated severity.
    pub severities: BTreeSet<FindingSeverity>,
    pub include_unrated: bool,
    pub text: Option<String>,
    pub sort: FindingSort,
    pub limit: Option<usize>,
}

impl Default for FindingQuery {
    fn default() -> Self {
        Self {
            severities: BTreeSet::new(),
            include_unrated: true,
            text: None,
            sort: FindingSort::Newest,
            limit: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FindingScanStats {
    pub parsed_files: usize,
    pub cached_files: usize,
    pub bytes_read: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FindingScan {
    pub cards: Vec<FindingCard>,
    pub stats: FindingScanStats,
}

/// Inputs for creating one finding. The Markdown body and optional extension
/// metadata remain project-authored; Corpus supplies only fields it can prove.
#[derive(Debug, Clone, PartialEq)]
pub struct NewFinding {
    pub title: String,
    pub severity: FindingSeverity,
    pub detail: String,
    pub timestamp: u64,
    pub oracle_verified: bool,
    pub oracle_output: String,
    /// Relative beneath `findings/`. A `.md` path names the file; a path with
    /// no extension (or a trailing slash) names a containing directory.
    pub path: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub run_log: Option<String>,
    pub actor: Option<String>,
    pub source_pins: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingWriteResult {
    /// Relative to the project corpus, beginning `findings/`.
    pub path: PathBuf,
    /// Extension `id` when supplied, otherwise the relative path.
    pub reference: String,
    pub bytes_written: u64,
}

#[derive(Debug, Clone)]
struct CachedFindingFile {
    modified: Option<SystemTime>,
    len: u64,
    card: FindingCard,
}

/// Parsed cards keyed by the file identity that invalidates their projection.
#[derive(Debug, Clone, Default)]
pub struct FindingIndexCache {
    files: BTreeMap<PathBuf, CachedFindingFile>,
}

impl Store {
    /// Create one finding without overwriting an existing artifact.
    ///
    /// The optional path is scoped beneath `findings/`; callers may choose
    /// any nested organization without turning severity into a directory
    /// convention. Corpus serializes the narrow rendering contract and leaves
    /// all non-reserved extension metadata intact.
    pub fn write_finding(&self, project: &str, finding: &NewFinding) -> Result<FindingWriteResult> {
        let title = finding.title.trim();
        if title.is_empty() {
            return Err(Error::Store("finding title is empty".into()));
        }
        let slug = match slugify(title) {
            slug if slug.is_empty() => "finding".to_string(),
            slug => slug,
        };
        for key in finding.metadata.keys() {
            if FINDING_RESERVED_KEYS.contains(&key.as_str()) {
                return Err(Error::Store(format!(
                    "finding metadata key {key:?} is reserved by Corpus"
                )));
            }
            if key.trim().is_empty() {
                return Err(Error::Store(
                    "finding metadata contains an empty key".into(),
                ));
            }
        }

        let requested = finding.path.as_deref().map(str::trim);
        if requested.is_some_and(|path| path.contains('\\')) {
            return Err(Error::Store(
                "finding path must use forward slashes and stay beneath findings/".into(),
            ));
        }
        let directory_hint = requested.is_some_and(|path| path.ends_with('/'));
        let requested_path = requested.filter(|path| !path.is_empty()).map(Path::new);
        let extension = requested_path
            .and_then(Path::extension)
            .and_then(|value| value.to_str());
        let exact_file = extension == Some("md") && !directory_hint;
        if extension.is_some() && !exact_file && !directory_hint {
            return Err(Error::Store(
                "a finding file path must use the .md extension".into(),
            ));
        }

        let generated_name = format!("{}-{slug}.md", finding.timestamp);
        let relative = match (requested_path, exact_file) {
            (Some(path), true) => PathBuf::from("findings").join(path),
            (Some(path), false) => PathBuf::from("findings").join(path).join(&generated_name),
            (None, _) => PathBuf::from("findings").join(&generated_name),
        };

        let mut frontmatter = crate::yaml::Mapping::new();
        for (key, value) in &finding.metadata {
            frontmatter.insert(
                crate::yaml::Value::String(key.clone()),
                crate::yaml::to_value(value)?,
            );
        }
        insert_yaml(&mut frontmatter, "title", title)?;
        insert_yaml(&mut frontmatter, "severity", finding.severity.as_str())?;
        insert_yaml(&mut frontmatter, "timestamp", finding.timestamp)?;
        insert_yaml(
            &mut frontmatter,
            "sensitivity",
            Sensitivity::Embargoed.as_str(),
        )?;
        insert_yaml(&mut frontmatter, "oracle_verified", finding.oracle_verified)?;
        if let Some(run_log) = finding
            .run_log
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            insert_yaml(&mut frontmatter, "run_log", run_log)?;
        }
        if let Some(actor) = finding
            .actor
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            insert_yaml(&mut frontmatter, "actor", actor)?;
        }
        if let Some(source_pins) = finding.source_pins.as_ref() {
            insert_yaml(&mut frontmatter, "source_pins", source_pins)?;
        }

        let yaml = crate::yaml::to_string(&frontmatter)?;
        let mut body = format!("---\n{yaml}---\n\n## Detail\n\n{}\n", finding.detail);
        if !finding.oracle_output.is_empty() {
            body.push_str("\n## Oracle output at report time\n\n");
            for line in finding.oracle_output.lines() {
                body.push_str("    ");
                body.push_str(line);
                body.push('\n');
            }
        }

        let path = write_finding_exclusive(self, project, &relative, &body, !exact_file)?;
        let reference = finding
            .metadata
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| path_text(&path));
        Ok(FindingWriteResult {
            path,
            reference,
            bytes_written: body.len() as u64,
        })
    }
}

fn insert_yaml(mapping: &mut crate::yaml::Mapping, key: &str, value: impl Serialize) -> Result<()> {
    mapping.insert(
        crate::yaml::Value::String(key.to_string()),
        crate::yaml::to_value(value)?,
    );
    Ok(())
}

fn write_finding_exclusive(
    store: &Store,
    project: &str,
    relative: &Path,
    content: &str,
    generated: bool,
) -> Result<PathBuf> {
    for collision in 0..10_000_u32 {
        let candidate = match collision {
            0 => relative.to_path_buf(),
            _ if generated => {
                let stem = relative
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        Error::Store(format!(
                            "finding path has no UTF-8 filename: {}",
                            relative.display()
                        ))
                    })?;
                relative.with_file_name(format!("{stem}-{collision}.md"))
            }
            _ => {
                return Err(Error::Store(format!(
                    "{} already exists — finding_write never overwrites an artifact",
                    relative.display()
                )))
            }
        };
        let rel = candidate.to_str().ok_or_else(|| {
            Error::Store(format!(
                "finding path is not valid UTF-8: {}",
                candidate.display()
            ))
        })?;
        let mut destination = store.resolve_corpus_entry(project, rel, EntryAccess::Destination)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        // Re-resolve after parent creation so a pre-existing/planted link is
        // checked at the deepest possible boundary before opening the file.
        destination = store.resolve_corpus_entry(project, rel, EntryAccess::Destination)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(content.as_bytes()) {
                    drop(file);
                    let _ = fs::remove_file(&destination);
                    return Err(error.into());
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && generated => {
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::Store(format!(
                    "{} already exists — finding_write never overwrites an artifact",
                    candidate.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::Store(format!(
        "could not allocate a collision-free finding name for {}",
        relative.display()
    )))
}

/// Fresh, uncached discovery in default newest-first order.
pub fn finding_cards(store: &Store, project: &str) -> Result<Vec<FindingCard>> {
    let mut cache = FindingIndexCache::default();
    Ok(scan_findings_cached(store, project, &mut cache, || false)?.cards)
}

/// Read one Markdown finding by its corpus-relative path. This is the CLI
/// detail seam; MCP intentionally keeps using the generic `corpus_read` tool.
pub fn read_finding(store: &Store, project: &str, relative: &str) -> Result<String> {
    let path = Path::new(relative);
    if path.extension().and_then(|value| value.to_str()) != Some("md")
        || path.components().next() != Some(Component::Normal(std::ffi::OsStr::new("findings")))
    {
        return Err(Error::Store(
            "finding path must be a .md entry beginning findings/".into(),
        ));
    }
    let resolved = store.resolve_corpus_entry(project, relative, EntryAccess::Read)?;
    if !resolved.is_file() {
        return Err(Error::Store(format!("finding is not a file: {relative}")));
    }
    Ok(fs::read_to_string(resolved)?)
}

/// Cached, cancellable discovery. Cache mutation is transactional: cancellation
/// or an I/O error leaves the caller's last good cache untouched.
pub fn scan_findings_cached<F>(
    store: &Store,
    project: &str,
    cache: &mut FindingIndexCache,
    mut cancelled: F,
) -> Result<FindingScan>
where
    F: FnMut() -> bool,
{
    if !store.project_dir(project).join("project.yaml").is_file() {
        return Err(Error::Store(format!("project not found: {project}")));
    }
    let findings = store.project_corpus_dir(project).join("findings");
    if !findings.exists() {
        cache.files.clear();
        return Ok(FindingScan::default());
    }
    let root_meta = fs::symlink_metadata(&findings)?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(Error::Store(format!(
            "finding root is not a real directory: {}",
            findings.display()
        )));
    }
    let canonical_root = findings.canonicalize()?;
    let corpus_root = store.project_corpus_dir(project);
    let mut next = cache.clone();
    let mut seen = BTreeSet::new();
    let mut cards = Vec::new();
    let mut stats = FindingScanStats::default();
    let mut pending = vec![findings];

    while let Some(dir) = pending.pop() {
        if cancelled() {
            return Err(Error::Store("finding scan cancelled".into()));
        }
        let mut entries = fs::read_dir(&dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if cancelled() {
                return Err(Error::Store("finding scan cancelled".into()));
            }
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("md")
            {
                continue;
            }

            match path.canonicalize() {
                Ok(canonical) if canonical.starts_with(&canonical_root) => {}
                _ => continue,
            }
            let relative = path.strip_prefix(&corpus_root).map_err(|_| {
                Error::Store(format!("finding escaped corpus root: {}", path.display()))
            })?;
            let cache_key = relative.to_path_buf();
            let metadata = entry.metadata()?;
            let modified = metadata.modified().ok();
            let len = metadata.len();
            seen.insert(cache_key.clone());

            let current = next
                .files
                .get(&cache_key)
                .is_some_and(|cached| cached.modified == modified && cached.len == len);
            let card = if current {
                stats.cached_files += 1;
                next.files
                    .get(&cache_key)
                    .expect("checked above")
                    .card
                    .clone()
            } else {
                stats.parsed_files += 1;
                let (prefix, truncated, invalid_utf8, bytes_read, unreadable) = read_prefix(&path);
                stats.bytes_read += bytes_read;
                let card = project_card(
                    relative,
                    &prefix,
                    modified,
                    truncated,
                    invalid_utf8,
                    unreadable,
                );
                next.files.insert(
                    cache_key,
                    CachedFindingFile {
                        modified,
                        len,
                        card: card.clone(),
                    },
                );
                card
            };
            cards.push(card);
        }
    }

    next.files.retain(|path, _| seen.contains(path));
    sort_cards(&mut cards, FindingSort::Newest);
    *cache = next;
    Ok(FindingScan { cards, stats })
}

/// Filter and order cards without touching the filesystem.
pub fn query_findings(cards: &[FindingCard], query: &FindingQuery) -> Vec<FindingCard> {
    let needle = query
        .text
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let mut found: Vec<FindingCard> = cards
        .iter()
        .filter(|card| match card.severity {
            Some(severity) => query.severities.is_empty() || query.severities.contains(&severity),
            None => query.include_unrated,
        })
        .filter(|card| {
            let Some(needle) = needle.as_deref() else {
                return true;
            };
            card.title.to_lowercase().contains(needle)
                || card.reference.to_lowercase().contains(needle)
                || path_text(&card.path).to_lowercase().contains(needle)
        })
        .cloned()
        .collect();
    sort_cards(&mut found, query.sort);
    if let Some(limit) = query.limit {
        found.truncate(limit);
    }
    found
}

fn sort_cards(cards: &mut [FindingCard], sort: FindingSort) {
    cards.sort_by(|left, right| match sort {
        FindingSort::Newest => right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| left.path.cmp(&right.path)),
        FindingSort::Severity => severity_rank(left.severity)
            .cmp(&severity_rank(right.severity))
            .then_with(|| right.timestamp.cmp(&left.timestamp))
            .then_with(|| left.path.cmp(&right.path)),
    });
}

fn severity_rank(severity: Option<FindingSeverity>) -> u8 {
    match severity {
        Some(FindingSeverity::Critical) => 0,
        Some(FindingSeverity::High) => 1,
        Some(FindingSeverity::Medium) => 2,
        Some(FindingSeverity::Low) => 3,
        None => 4,
    }
}

fn read_prefix(path: &Path) -> (String, bool, bool, u64, bool) {
    let Ok(file) = File::open(path) else {
        return (String::new(), false, false, 0, true);
    };
    let mut bytes = Vec::with_capacity(FINDING_PREFIX_LIMIT.min(8 * 1024));
    let mut reader: Take<File> = file.take((FINDING_PREFIX_LIMIT + 1) as u64);
    if reader.read_to_end(&mut bytes).is_err() {
        return (String::new(), false, false, 0, true);
    }
    let bytes_read = bytes.len() as u64;
    let truncated = bytes.len() > FINDING_PREFIX_LIMIT;
    bytes.truncate(FINDING_PREFIX_LIMIT);
    match String::from_utf8(bytes) {
        Ok(text) => (text, truncated, false, bytes_read, false),
        Err(error) => (
            String::from_utf8_lossy(error.as_bytes()).into_owned(),
            truncated,
            true,
            bytes_read,
            false,
        ),
    }
}

fn project_card(
    relative: &Path,
    prefix: &str,
    modified: Option<SystemTime>,
    truncated: bool,
    invalid_utf8: bool,
    unreadable: bool,
) -> FindingCard {
    let mut warnings = Vec::new();
    if invalid_utf8 {
        warnings.push(FindingWarning::InvalidUtf8);
    }
    if unreadable {
        warnings.push(FindingWarning::Unreadable);
    }

    let started_frontmatter = prefix.starts_with("---\n");
    let split = frontmatter::split(prefix);
    let (mapping, body) = match split {
        Ok((Some(mapping), body)) => (Some(mapping), body),
        Ok((None, _)) if started_frontmatter => {
            warnings.push(FindingWarning::InvalidFrontmatter);
            (None, body_after_fence(prefix))
        }
        Ok((None, body)) => {
            warnings.push(FindingWarning::MissingFrontmatter);
            (None, body)
        }
        Err(_) => {
            warnings.push(FindingWarning::InvalidFrontmatter);
            (None, body_after_fence(prefix))
        }
    };

    let title_value = mapping
        .as_ref()
        .and_then(|fm| nonempty(frontmatter::get_str(fm, "title")));
    let name_value = mapping
        .as_ref()
        .and_then(|fm| nonempty(frontmatter::get_str(fm, "name")));
    let heading = first_heading(body);
    let (title, title_source) = if let Some(title) = title_value {
        (title, FindingTitleSource::Title)
    } else if let Some(name) = name_value {
        (name, FindingTitleSource::Name)
    } else if let Some(heading) = heading {
        (heading, FindingTitleSource::Heading)
    } else {
        (
            relative
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("finding")
                .to_string(),
            FindingTitleSource::FileName,
        )
    };
    if truncated && matches!(title_source, FindingTitleSource::FileName) {
        warnings.push(FindingWarning::PrefixLimit);
    }

    let severity = match mapping
        .as_ref()
        .and_then(|fm| frontmatter::get_str(fm, "severity"))
    {
        Some(value) => match FindingSeverity::parse(value.trim()) {
            Some(severity) => Some(severity),
            None => {
                warnings.push(FindingWarning::InvalidSeverity);
                None
            }
        },
        None => {
            warnings.push(FindingWarning::MissingSeverity);
            None
        }
    };

    let declared_timestamp = mapping
        .as_ref()
        .and_then(|fm| frontmatter_u64(fm, "timestamp"));
    let filename_timestamp = filename_epoch(relative);
    let modified_timestamp = modified.and_then(system_time_seconds);
    let (timestamp, time_source) = if let Some(timestamp) = declared_timestamp {
        (Some(timestamp), Some(FindingTimeSource::Timestamp))
    } else if let Some(timestamp) = filename_timestamp {
        (Some(timestamp), Some(FindingTimeSource::FileName))
    } else if let Some(timestamp) = modified_timestamp {
        (Some(timestamp), Some(FindingTimeSource::Modified))
    } else {
        (None, None)
    };

    let id = mapping
        .as_ref()
        .and_then(|fm| nonempty(frontmatter::get_str(fm, "id")));
    let (reference, reference_source) = match id {
        Some(id) => (id, FindingReferenceSource::Id),
        None => (path_text(relative), FindingReferenceSource::Path),
    };
    let status = mapping
        .as_ref()
        .and_then(|fm| nonempty(frontmatter::get_str(fm, "status")));
    let oracle_verified = mapping
        .as_ref()
        .and_then(|fm| frontmatter::get(fm, "oracle_verified"))
        .and_then(crate::yaml::Value::as_bool);
    let sensitivity =
        mapping
            .as_ref()
            .and_then(|fm| match Sensitivity::from_frontmatter(fm, "findings") {
                Ok(value) => Some(value),
                Err(_) => {
                    warnings.push(FindingWarning::InvalidSensitivity);
                    None
                }
            });

    FindingCard {
        path: relative.to_path_buf(),
        title,
        title_source,
        severity,
        timestamp,
        time_source,
        reference,
        reference_source,
        status,
        oracle_verified,
        sensitivity,
        warnings,
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn first_heading(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
    })
}

fn body_after_fence(prefix: &str) -> &str {
    prefix
        .strip_prefix("---\n")
        .and_then(|after| after.find("\n---\n").map(|end| &after[end + 5..]))
        .unwrap_or(prefix)
}

fn frontmatter_u64(mapping: &crate::yaml::Mapping, key: &str) -> Option<u64> {
    let value = frontmatter::get(mapping, key)?;
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
        .or_else(|| value.as_str().and_then(|number| number.parse().ok()))
}

fn filename_epoch(path: &Path) -> Option<u64> {
    path.file_name()
        .and_then(|value| value.to_str())
        .and_then(|name| name.split_once('-'))
        .and_then(|(prefix, _)| prefix.parse().ok())
}

fn system_time_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_secs())
}

fn path_text(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}
