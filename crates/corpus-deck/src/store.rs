//! Store reader: `store/` directories + frontmatter → structs.
//!
//! The corpus store is plain Markdown files under a `store/` root, one
//! category per subdirectory (`hypotheses`, `techniques`, `findings`,
//! `attacks`, `runs`). Some carry YAML frontmatter (findings, techniques,
//! hypotheses); others are pure prose (attacks) or log text (runs). This
//! module reads paths, parsed frontmatter (as a generic JSON map), and
//! raw markdown, staying tolerant of files with no frontmatter.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One store category (a `store/<name>/` directory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Hypotheses,
    Techniques,
    Findings,
    Attacks,
    Runs,
}

impl Category {
    /// All categories, in tree display order (workflow order, not
    /// alphabetical: idea → technique → result → probe → evidence).
    pub const ALL: [Category; 5] = [
        Category::Hypotheses,
        Category::Techniques,
        Category::Findings,
        Category::Attacks,
        Category::Runs,
    ];

    /// Directory name under the store root.
    pub fn dir_name(self) -> &'static str {
        match self {
            Category::Hypotheses => "hypotheses",
            Category::Techniques => "techniques",
            Category::Findings => "findings",
            Category::Attacks => "attacks",
            Category::Runs => "runs",
        }
    }

    /// Human label shown in the tree.
    pub fn label(self) -> &'static str {
        match self {
            Category::Hypotheses => "Hypotheses",
            Category::Techniques => "Techniques",
            Category::Findings => "Findings",
            Category::Attacks => "Attacks",
            Category::Runs => "Runs",
        }
    }
}

/// One store entry (one logical item in a category).
#[derive(Debug, Clone)]
pub struct Entry {
    /// Category this entry belongs to.
    pub category: Category,
    /// Stem (filename without extension); the stable identity.
    pub stem: String,
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Parsed frontmatter (empty object if none/none invalid).
    pub frontmatter: Value,
    /// Full raw file content (markdown body for docs, raw text for runs).
    pub body: String,
}

impl Entry {
    /// Read a `key` from frontmatter as a string.
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.frontmatter.get(key).and_then(Value::as_str)
    }

    /// Display title: frontmatter `title`/`name` if present, else a
    /// cleaned-up stem (timestamp prefix and dashes stripped).
    pub fn title(&self) -> String {
        if let Some(title) = self.meta("title").or_else(|| self.meta("name")) {
            return title.to_string();
        }
        // Run/finding stems look like "1786392937-operator-call-target-info":
        // strip the leading epoch, humanize the rest.
        let stripped = self
            .stem
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
        let stem = if stripped.is_empty() {
            &self.stem
        } else {
            stripped
        };
        stem.replace('-', " ")
    }

    /// Epoch timestamp from frontmatter or filename prefix, if any.
    pub fn timestamp(&self) -> Option<u64> {
        self.meta("timestamp")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                self.stem
                    .split('-')
                    .next()
                    .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
                    .and_then(|s| s.parse().ok())
            })
    }
}

/// The whole store snapshot that a view renders.
#[derive(Debug, Default)]
pub struct Store {
    /// All entries, one per category, in display order.
    pub entries: Vec<Entry>,
}

impl Store {
    /// The store root directory.
    pub fn root() -> PathBuf {
        std::env::var("CORPUS_STORE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                PathBuf::from(format!("{home}/Sites/corpus/store"))
            })
    }

    /// Scan the store root, populating each category.
    pub fn scan() -> Self {
        let mut entries = Vec::new();
        for category in Category::ALL {
            entries.extend(read_category(&Self::root(), category));
        }
        Self { entries }
    }

    /// Entries filtered to a single category.
    pub fn of(&self, category: Category) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |e| e.category == category)
    }
}

/// Read one `store/<category>/` directory into display-ordered entries.
fn read_category(root: &Path, category: Category) -> Vec<Entry> {
    let dir = root.join(category.dir_name());
    let Ok(read) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter_map(|p| collect_files(&p))
        .flatten()
        // An attack is a DIRECTORY (attack.md + run.sh) — one entry per
        // directory, anchored on the doc; the script is a detail, not a
        // sibling entry.
        .filter(|p| category != Category::Attacks || p.file_name().is_some_and(|n| n == "attack.md"))
        .collect();
    // Timestamp-prefixed categories read newest-first; the rest A–Z.
    match category {
        Category::Findings | Category::Runs => paths.sort_by(|a, b| b.cmp(a)),
        _ => paths.sort(),
    }

    let mut entries = Vec::new();
    for path in paths {
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let (frontmatter, body) = split_frontmatter(&body);
        // Attack entries are keyed by their directory slug, not "attack".
        let stem = if category == Category::Attacks {
            path.parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        entries.push(Entry {
            category,
            stem,
            path,
            frontmatter,
            body,
        });
    }
    entries
}

/// Recursively yield all regular files under `path` (file itself included).
fn collect_files(path: &Path) -> Option<Vec<PathBuf>> {
    if path.is_file() {
        return Some(vec![path.to_path_buf()]);
    }
    if path.is_dir() {
        let read = fs::read_dir(path).ok()?;
        let mut out = Vec::new();
        for entry in read.flatten() {
            if let Some(files) = collect_files(&entry.path()) {
                out.extend(files);
            }
        }
        return Some(out);
    }
    None
}

/// Split leading YAML frontmatter (`---` block) from the body.
/// Tolerant: no frontmatter, or a malformed block, yields raw body with an
/// empty frontmatter map (never fails the read).
fn split_frontmatter(raw: &str) -> (Value, String) {
    let Some(trimmed) = raw.strip_prefix("---") else {
        return (Value::Object(Default::default()), raw.to_string());
    };
    let Some(end) = trimmed.find("\n---") else {
        return (Value::Object(Default::default()), raw.to_string());
    };
    let yaml = &trimmed[..end];
    let rest = &trimmed[end + 4..]; // skip the closing `---`
    match serde_yaml::from_str::<Value>(yaml) {
        Ok(value) if value.is_object() => (value, rest.to_string()),
        _ => (Value::Object(Default::default()), raw.to_string()),
    }
}

/// Strip ANSI escape codes from a body (run logs are full of them).
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ params... final_byte (final in 0x40..=0x7E).
            let intro = chars.next();
            if intro == Some('[') {
                for c in chars.by_ref() {
                    let b = c as u8;
                    if (0x40..=0x7E).contains(&b) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Format epoch seconds as `YYYY-MM-DD HH:MM` (UTC) without a date crate.
pub fn format_epoch(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let secs = epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// Howard Hinnant's civil-from-days algorithm (public domain).
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter() {
        let raw = "---\ntitle: Hello\nseverity: high\n---\n\n## Body\n";
        let (fm, body) = split_frontmatter(raw);
        assert_eq!(fm["title"], serde_json::json!("Hello"));
        assert_eq!(fm["severity"], serde_json::json!("high"));
        assert_eq!(body.trim(), "## Body");
    }

    #[test]
    fn tolerates_no_frontmatter() {
        let (fm, body) = split_frontmatter("# just prose\n");
        assert!(fm.as_object().unwrap().is_empty());
        assert_eq!(body, "# just prose\n");
    }

    #[test]
    fn strips_ansi() {
        let text = "\u{1b}[0mhello \u{1b}[90mworld\u{1b}[0m";
        assert_eq!(strip_ansi(text), "hello world");
    }

    #[test]
    fn formats_epoch() {
        // 2026-08-10T19:18:49Z
        assert_eq!(format_epoch(1_786_389_529), "2026-08-10 19:18Z");
    }
}
