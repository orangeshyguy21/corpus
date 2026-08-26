//! Compact usage snapshots and project cost aggregation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filesystem::atomic_write;
use crate::store::{validate_slug, Store};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostRow {
    pub provider: String,
    pub model: String,
    pub messages: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub inference_ms: u64,
    pub timed_messages: u64,
    pub cost: f64,
}

pub const USAGE_SNAPSHOT_VERSION: u32 = 1;

/// Compact cumulative accounting for one OpenCode session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub version: u32,
    pub session_id: String,
    pub captured_at: u64,
    pub source: String,
    pub rows: Vec<CostRow>,
}

impl UsageSnapshot {
    pub fn report(&self) -> CostReport {
        let mut report = CostReport {
            rows: self.rows.clone(),
            ..CostReport::default()
        };
        report.tokens = report
            .rows
            .iter()
            .map(|row| {
                row.tokens_input
                    .saturating_add(row.tokens_output)
                    .saturating_add(row.tokens_reasoning)
            })
            .sum();
        report.inference_ms = report.rows.iter().map(|row| row.inference_ms).sum();
        report.timed_messages = report.rows.iter().map(|row| row.timed_messages).sum();
        report.cost = report.rows.iter().map(|row| row.cost).sum();
        report
    }
}

#[derive(Debug, Clone, Default)]
pub struct CostReport {
    pub rows: Vec<CostRow>,
    pub tokens: u64,
    pub inference_ms: u64,
    pub timed_messages: u64,
    pub cost: f64,
    pub accounted_sessions: u64,
    pub legacy_sessions: u64,
    pub last_updated: Option<u64>,
}

#[derive(Debug, Clone)]
struct CachedCostFile {
    modified: Option<std::time::SystemTime>,
    len: u64,
    report: CostReport,
}

#[derive(Debug, Clone, Default)]
pub struct CorpusCostCache {
    files: BTreeMap<PathBuf, CachedCostFile>,
}

impl Store {
    /// Derived accounting state, outside curator-authored corpus artifacts.
    pub fn project_usage_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("usage")
    }

    pub fn write_usage_snapshot(&self, project: &str, snapshot: &UsageSnapshot) -> Result<PathBuf> {
        validate_slug(project)?;
        if snapshot.session_id.is_empty()
            || snapshot.session_id.contains('/')
            || snapshot.session_id.contains('\\')
        {
            return Err(Error::Store(
                "usage snapshot has an invalid session id".into(),
            ));
        }
        let dir = self.project_usage_dir(project);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", snapshot.session_id));
        let bytes = serde_json::to_vec_pretty(snapshot)
            .map_err(|error| Error::Store(format!("usage snapshot: {error}")))?;
        atomic_write(&path, bytes)?;
        Ok(path)
    }

    /// Reduce historical transcript exports to compact snapshots.
    pub fn backfill_usage_snapshots(&self, project: &str) -> Result<usize> {
        let runs = self.project_corpus_dir(project).join("runs");
        if !runs.is_dir() {
            return Ok(0);
        }
        let mut written = 0;
        for entry in fs::read_dir(runs)? {
            let entry = entry?;
            let path = entry.path();
            let Some(session_id) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if path.extension().and_then(|value| value.to_str()) != Some("json")
                || self
                    .project_usage_dir(project)
                    .join(format!("{session_id}.json"))
                    .is_file()
            {
                continue;
            }
            let report = parse_cost_file(&path);
            if report.rows.is_empty() {
                continue;
            }
            let captured_at = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or_default();
            self.write_usage_snapshot(
                project,
                &UsageSnapshot {
                    version: USAGE_SNAPSHOT_VERSION,
                    session_id: session_id.to_string(),
                    captured_at,
                    source: "legacy-transcript".into(),
                    rows: report.rows,
                },
            )?;
            written += 1;
        }
        Ok(written)
    }
}

pub fn corpus_cost(store: &Store, project: &str) -> Result<CostReport> {
    corpus_cost_cached(store, project, &mut CorpusCostCache::default())
}

pub fn corpus_cost_cached(
    store: &Store,
    project: &str,
    cache: &mut CorpusCostCache,
) -> Result<CostReport> {
    let usage = store.project_usage_dir(project);
    let runs = store.project_corpus_dir(project).join("runs");
    if !usage.is_dir() && !runs.is_dir() {
        cache.files.clear();
        return Ok(CostReport::default());
    }
    let mut seen = BTreeSet::new();
    let mut snapshotted = BTreeSet::new();
    let usage_entries = usage.is_dir().then(|| fs::read_dir(&usage)).transpose()?;
    for entry in usage_entries.into_iter().flatten() {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
            snapshotted.insert(stem.to_string());
        }
        cache_cost_path(cache, &mut seen, path, parse_snapshot_file);
    }
    let run_entries = runs.is_dir().then(|| fs::read_dir(&runs)).transpose()?;
    for entry in run_entries.into_iter().flatten() {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        if path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|id| snapshotted.contains(id))
        {
            continue;
        }
        cache_cost_path(cache, &mut seen, path, parse_cost_file);
    }
    cache.files.retain(|path, _| seen.contains(path));
    Ok(merge_cost_reports(
        cache.files.values().map(|cached| &cached.report),
    ))
}

fn cache_cost_path(
    cache: &mut CorpusCostCache,
    seen: &mut BTreeSet<PathBuf>,
    path: PathBuf,
    parse: fn(&Path) -> CostReport,
) {
    let Ok(metadata) = fs::metadata(&path) else {
        return;
    };
    let modified = metadata.modified().ok();
    let len = metadata.len();
    seen.insert(path.clone());
    if !cache
        .files
        .get(&path)
        .is_some_and(|cached| cached.modified == modified && cached.len == len)
    {
        cache.files.insert(
            path.clone(),
            CachedCostFile {
                modified,
                len,
                report: parse(&path),
            },
        );
    }
}

fn parse_snapshot_file(path: &Path) -> CostReport {
    fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<UsageSnapshot>(&raw).ok())
        .filter(|snapshot| snapshot.version == USAGE_SNAPSHOT_VERSION)
        .map(|snapshot| {
            let legacy = snapshot.source == "legacy-transcript";
            let captured_at = snapshot.captured_at;
            let mut report = snapshot.report();
            report.accounted_sessions = 1;
            report.legacy_sessions = u64::from(legacy);
            report.last_updated = Some(captured_at);
            report
        })
        .unwrap_or_default()
}

fn parse_cost_file(path: &Path) -> CostReport {
    let mut report = CostReport::default();
    let mut rows = BTreeMap::<(String, String), CostRow>::new();
    if let Ok(raw) = fs::read_to_string(path) {
        if let Ok(document) = serde_json::from_str::<serde_json::Value>(&raw) {
            for message in document
                .get("messages")
                .and_then(|messages| messages.as_array())
                .into_iter()
                .flatten()
            {
                let info = message.get("info").cloned().unwrap_or_default();
                if info.get("role").and_then(|role| role.as_str()) != Some("assistant") {
                    continue;
                }
                let provider = info
                    .get("providerID")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let model = info
                    .get("modelID")
                    .and_then(|value| value.as_str())
                    .and_then(|model| model.rsplit('/').next())
                    .unwrap_or("unknown")
                    .to_string();
                let row = rows
                    .entry((provider.clone(), model.clone()))
                    .or_insert_with(|| CostRow {
                        provider,
                        model,
                        ..CostRow::default()
                    });
                let tokens = info.get("tokens").cloned().unwrap_or_default();
                let take = |value: &serde_json::Value, key: &str| {
                    value
                        .get(key)
                        .and_then(|number| number.as_u64())
                        .unwrap_or(0)
                };
                let cache = tokens.get("cache").cloned().unwrap_or_default();
                row.messages += 1;
                row.tokens_input += take(&tokens, "input");
                row.tokens_output += take(&tokens, "output");
                row.tokens_reasoning += take(&tokens, "reasoning");
                row.cache_read += take(&cache, "read");
                row.cache_write += take(&cache, "write");
                if let Some(inference_ms) = message_inference_ms(message) {
                    row.inference_ms = row.inference_ms.saturating_add(inference_ms);
                    row.timed_messages += 1;
                    report.inference_ms = report.inference_ms.saturating_add(inference_ms);
                    report.timed_messages += 1;
                }
                row.cost += info
                    .get("cost")
                    .and_then(|cost| cost.as_f64())
                    .unwrap_or(0.0);
                report.tokens = report.tokens.saturating_add(
                    take(&tokens, "input")
                        .saturating_add(take(&tokens, "output"))
                        .saturating_add(take(&tokens, "reasoning")),
                );
            }
        }
    }
    report.rows = rows.into_values().collect();
    report.cost = report.rows.iter().map(|row| row.cost).sum();
    if !report.rows.is_empty() {
        report.accounted_sessions = 1;
        report.legacy_sessions = 1;
    }
    report
}

fn message_inference_ms(message: &serde_json::Value) -> Option<u64> {
    let time = message.get("info")?.get("time")?;
    let created = time.get("created")?.as_u64()?;
    let completed = time.get("completed")?.as_u64()?;
    if completed < created {
        return None;
    }
    let mut intervals = message
        .get("parts")
        .and_then(|parts| parts.as_array())
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(|kind| kind.as_str()) == Some("tool"))
        .filter_map(|part| {
            let time = part.get("state")?.get("time")?;
            let start = time.get("start")?.as_u64()?.max(created);
            let end = time.get("end")?.as_u64()?.min(completed);
            (end > start).then_some((start, end))
        })
        .collect::<Vec<_>>();
    intervals.sort_unstable();

    let mut tool_ms = 0_u64;
    let mut merged: Option<(u64, u64)> = None;
    for (start, end) in intervals {
        match merged {
            Some((merged_start, merged_end)) if start <= merged_end => {
                merged = Some((merged_start, merged_end.max(end)));
            }
            Some((merged_start, merged_end)) => {
                tool_ms = tool_ms.saturating_add(merged_end - merged_start);
                merged = Some((start, end));
            }
            None => merged = Some((start, end)),
        }
    }
    if let Some((start, end)) = merged {
        tool_ms = tool_ms.saturating_add(end - start);
    }
    Some((completed - created).saturating_sub(tool_ms))
}

fn merge_cost_reports<'a>(reports: impl Iterator<Item = &'a CostReport>) -> CostReport {
    let mut report = CostReport::default();
    let mut rows = BTreeMap::<(String, String), CostRow>::new();
    for source in reports {
        report.tokens += source.tokens;
        report.accounted_sessions += source.accounted_sessions;
        report.legacy_sessions += source.legacy_sessions;
        report.last_updated = report.last_updated.max(source.last_updated);
        for source_row in &source.rows {
            let row = rows
                .entry((source_row.provider.clone(), source_row.model.clone()))
                .or_insert_with(|| CostRow {
                    provider: source_row.provider.clone(),
                    model: source_row.model.clone(),
                    ..CostRow::default()
                });
            row.messages += source_row.messages;
            row.tokens_input += source_row.tokens_input;
            row.tokens_output += source_row.tokens_output;
            row.tokens_reasoning += source_row.tokens_reasoning;
            row.cache_read += source_row.cache_read;
            row.cache_write += source_row.cache_write;
            row.inference_ms += source_row.inference_ms;
            row.timed_messages += source_row.timed_messages;
            row.cost += source_row.cost;
        }
    }
    report.rows = rows.into_values().collect();
    report.inference_ms = report.rows.iter().map(|row| row.inference_ms).sum();
    report.timed_messages = report.rows.iter().map(|row| row.timed_messages).sum();
    report.rows.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    report.cost = report.rows.iter().map(|row| row.cost).sum();
    report
}

#[cfg(test)]
#[path = "accounting/tests.rs"]
mod tests;
