//! Corpus projection and refresh coordination.

use std::time::Duration;

use corpus_core::{CorpusStats, CostReport, FindingIndexCache};

use super::{AppJobOutput, AppState, CorpusSnapshot, FindingDiscovery, FindingSnapshot};
use crate::jobs::{JobKind, JobScope};

impl AppState {
    pub(super) fn corpus_revision(&self, project: &str) -> u64 {
        self.corpus_revisions.get(project).copied().unwrap_or(0)
    }

    pub(super) fn corpus_job_scope(&self, project: &str) -> JobScope {
        let mut scope = self.job_scope(project, None);
        scope.corpus_revision = Some(self.corpus_revision(project));
        scope
    }

    /// Mark corpus projections dirty after a known local mutation. Filesystem
    /// notifications call the same seam for external writes.
    pub fn note_corpus_mutation(&mut self, project: &str) {
        let revision = self
            .corpus_revisions
            .entry(project.to_string())
            .or_default();
        *revision = revision.saturating_add(1);
        if self.effective_project().as_deref() == Some(project) {
            self.corpus_polled_at = None;
        }
    }

    pub(super) fn retry_stale_corpus_job(&mut self, kind: JobKind, scope: &JobScope) -> bool {
        let Some(captured_revision) = scope.corpus_revision else {
            return false;
        };
        if !matches!(
            kind,
            JobKind::ProjectScope | JobKind::CorpusSummary | JobKind::CorpusCost
        ) || captured_revision == self.corpus_revision(&scope.project)
        {
            return false;
        }
        match kind {
            JobKind::CorpusSummary => self.schedule_corpus_refresh(&scope.project, false),
            JobKind::CorpusCost => self.schedule_corpus_refresh(&scope.project, true),
            JobKind::ProjectScope => {
                self.corpus_polled_at = None;
                self.poll_project_scope();
            }
            _ => unreachable!(),
        }
        true
    }

    pub(super) fn apply_findings(&mut self, project: &str, snapshot: FindingSnapshot) {
        if self.findings_project.as_deref() != Some(project) {
            return;
        }
        self.finding_index_cache = snapshot.cache;
        self.findings = FindingDiscovery::Ready(snapshot.cards);
    }

    pub(super) fn fail_findings(&mut self, project: &str, message: &str) {
        if self.findings_project.as_deref() != Some(project) {
            return;
        }
        let last_good = match std::mem::take(&mut self.findings) {
            FindingDiscovery::Ready(cards) => cards,
            FindingDiscovery::Failed { last_good, .. } => last_good,
            FindingDiscovery::Loading => Vec::new(),
        };
        self.findings = FindingDiscovery::Failed {
            message: message.to_string(),
            last_good,
        };
    }

    /// Re-walk a project's corpus, findings, mission logs, and cost report.
    pub fn refresh_corpus_stats(&mut self, project: &str) {
        self.prepare_findings_project(project);
        if self.jobs.is_some() {
            self.schedule_corpus_refresh(project, true);
            return;
        }
        let _ = self.store.backfill_usage_snapshots(project);
        self.corpus_stats = corpus_core::corpus_stats(&self.store, project).ok();
        self.mission_logs = corpus_core::mission_logs(&self.store, project).unwrap_or_default();
        self.corpus_cost =
            corpus_core::corpus_cost_cached(&self.store, project, &mut self.corpus_cost_cache).ok();
        self.refresh_findings_sync(project);
        self.corpus_stats_project = Some(project.to_string());
        self.corpus_polled_at = Some(self.clock.monotonic_now());
    }

    pub(super) fn refresh_corpus_summary(&mut self, project: &str) {
        self.prepare_findings_project(project);
        if self.jobs.is_some() {
            self.schedule_corpus_refresh(project, false);
            return;
        }
        self.corpus_stats = corpus_core::corpus_stats(&self.store, project).ok();
        self.mission_logs = corpus_core::mission_logs(&self.store, project).unwrap_or_default();
        self.refresh_findings_sync(project);
        self.corpus_stats_project = Some(project.to_string());
        self.corpus_polled_at = Some(self.clock.monotonic_now());
    }

    fn schedule_corpus_refresh(&mut self, project: &str, include_cost: bool) {
        self.prepare_findings_project(project);
        let scope = self.corpus_job_scope(project);
        self.corpus_polled_at = Some(self.clock.monotonic_now());
        let store = self.store.clone();
        let project = project.to_string();
        let mut cache = self.corpus_cost_cache.clone();
        let mut finding_cache = self.finding_index_cache.clone();
        self.jobs.as_mut().expect("installed above").start(
            if include_cost {
                JobKind::CorpusCost
            } else {
                JobKind::CorpusSummary
            },
            scope,
            Duration::from_secs(30),
            move |token| {
                store
                    .backfill_usage_snapshots(&project)
                    .map_err(|error| error.to_string())?;
                let stats = corpus_core::corpus_stats(&store, &project)
                    .map_err(|error| error.to_string())?;
                let logs = corpus_core::mission_logs(&store, &project)
                    .map_err(|error| error.to_string())?;
                let cost = if include_cost {
                    Some((
                        corpus_core::corpus_cost_cached(&store, &project, &mut cache)
                            .map_err(|error| error.to_string())?,
                        cache,
                    ))
                } else {
                    None
                };
                let findings =
                    corpus_core::scan_findings_cached(&store, &project, &mut finding_cache, || {
                        token.is_cancelled()
                    })
                    .map_err(|error| error.to_string())?;
                Ok(AppJobOutput::CorpusSnapshot(CorpusSnapshot {
                    stats,
                    logs,
                    cost,
                    findings: FindingSnapshot {
                        cards: findings.cards,
                        cache: finding_cache,
                    },
                }))
            },
        );
    }

    pub(super) fn prepare_findings_project(&mut self, project: &str) {
        if self.findings_project.as_deref() == Some(project) {
            return;
        }
        self.findings_project = Some(project.to_string());
        self.findings = FindingDiscovery::Loading;
        self.finding_index_cache = FindingIndexCache::default();
    }

    fn refresh_findings_sync(&mut self, project: &str) {
        match corpus_core::scan_findings_cached(
            &self.store,
            project,
            &mut self.finding_index_cache,
            || false,
        ) {
            Ok(scan) => self.findings = FindingDiscovery::Ready(scan.cards),
            Err(error) => self.fail_findings(project, &error.to_string()),
        }
    }

    pub fn corpus_stats(&self) -> Option<&CorpusStats> {
        self.corpus_stats.as_ref()
    }

    pub fn finding_discovery(&self) -> &FindingDiscovery {
        &self.findings
    }

    pub fn mission_logs(&self) -> &[corpus_core::MissionLog] {
        &self.mission_logs
    }

    pub fn corpus_cost(&self) -> Option<&CostReport> {
        self.corpus_cost.as_ref()
    }
}
