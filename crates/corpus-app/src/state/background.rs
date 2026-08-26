//! Background-job runtime, invalidation, and result routing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use corpus_core::{Error, PluginStatus, SourceRevs};

use super::{
    load_launchable_mission, plugin_recovery_hint, AppJobOutput, AppState, BackgroundNotice,
    ModelDiscovery, PluginOperationState, RunId, RunPhaseKind, UNWATCHED_STORE_BACKSTOP,
    WATCHED_STORE_BACKSTOP,
};
use crate::file_watch::NotifyFileInvalidationSource;
use crate::jobs::{JobKind, JobScope, JobSet, JobTerminal};

/// Some lifecycle jobs complete their coordinator successfully while
/// carrying an operation-level failure that must remain visible/retryable.
/// Treating those as resolved first produces the characteristic error flash.
pub(super) fn successful_job_resolves_notice(terminal: &JobTerminal<AppJobOutput>) -> bool {
    match terminal {
        JobTerminal::Success(AppJobOutput::SessionMaintenance(maintenance)) => {
            maintenance.export_failure.is_none()
        }
        JobTerminal::Success(AppJobOutput::TeardownReady(teardown)) => teardown.error.is_none(),
        JobTerminal::Success(_) => true,
        _ => false,
    }
}

impl AppState {
    pub(super) fn store_backstop(&self) -> Duration {
        if self.file_invalidations.is_some() {
            WATCHED_STORE_BACKSTOP
        } else {
            UNWATCHED_STORE_BACKSTOP
        }
    }

    pub fn install_job_runtime(&mut self, context: eframe::egui::Context) {
        let wake = Arc::new(context.clone());
        self.jobs = Some(JobSet::new(wake.clone()));
        match NotifyFileInvalidationSource::new(self.store.projects_dir(), wake) {
            Ok(watcher) => self.file_invalidations = Some(Box::new(watcher)),
            Err(error) => {
                self.file_watch_warning = Some(format!(
                    "filesystem notifications unavailable ({error}); using timed reconciliation"
                ));
            }
        }
    }

    /// Drain coalesced filesystem hints. They only make reconciliation due;
    /// the callback never edits app state and a path is never treated as a
    /// precise run event.
    pub fn poll_file_invalidations(&mut self) -> Option<String> {
        let invalidations = self
            .file_invalidations
            .as_ref()
            .map(|source| source.take())
            .unwrap_or_default();
        let warning = self
            .file_watch_warning
            .take()
            .or(invalidations.warning.clone());
        if invalidations.is_empty() {
            return warning;
        }

        if invalidations.project_index {
            self.refresh();
        }
        if invalidations.all_projects {
            let projects: Vec<String> =
                self.projects.iter().map(|(slug, _)| slug.clone()).collect();
            for project in projects {
                self.note_corpus_mutation(&project);
            }
        } else {
            for project in &invalidations.corpus {
                self.note_corpus_mutation(project);
            }
        }
        let selected = self.effective_project();
        let applies = |projects: &BTreeSet<String>| {
            invalidations.all_projects
                || selected
                    .as_ref()
                    .is_some_and(|project| projects.contains(project))
        };
        if applies(&invalidations.metadata) || applies(&invalidations.corpus) {
            self.corpus_polled_at = None;
        }
        if applies(&invalidations.metadata) {
            self.launch_requests_polled_at = None;
            self.live_sessions_dirty = true;
        }
        if applies(&invalidations.activity) {
            self.session_activity_dirty = true;
            self.session_reconcile_due_at = Some(
                self.clock.monotonic_now() + Duration::from_secs(corpus_core::WORKING_WINDOW_SECS),
            );
        }
        warning
    }

    pub(super) fn job_scope(&self, project: &str, run_id: Option<RunId>) -> JobScope {
        let project_generation = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)
            .map(|(_, project)| project.corpus_generation)
            .unwrap_or(0);
        JobScope {
            project: project.to_string(),
            project_generation,
            corpus_revision: None,
            run_id,
        }
    }

    /// Apply all completed jobs that still belong to current state. The
    /// returned notices are rendered as toasts by `App`; discovery failures
    /// also remain in their field state so reopening a picker is explanatory.
    pub fn poll_background_jobs(&mut self) -> Vec<BackgroundNotice> {
        let Some(mut jobs) = self.jobs.take() else {
            return Vec::new();
        };
        let results = jobs.drain_applicable(|scope| self.job_scope_current(scope));
        self.jobs = Some(jobs);
        let mut notices = Vec::new();
        for result in results {
            if self.retry_stale_corpus_job(result.kind, &result.scope) {
                continue;
            }
            if successful_job_resolves_notice(&result.terminal) {
                notices.push(BackgroundNotice::resolved(result.kind));
            }
            match result.terminal {
                JobTerminal::Success(AppJobOutput::Plugins {
                    target,
                    project,
                    statuses,
                    leases,
                }) => {
                    self.plugin_probe_active = None;
                    self.plugin_probe_active_project = None;
                    if target == self.plugin_probe_target && project == self.plugin_probe_project {
                        self.plugins = merge_plugin_statuses(&self.plugins, statuses);
                        self.plugin_leases = leases;
                        self.plugins_loading = false;
                        self.plugins_error = None;
                    } else {
                        // A project/picker changed while the old global-keyed
                        // job was running. Its catalog is stale for health;
                        // now that the key is free, immediately probe the
                        // latest requested binding.
                        let desired = self.plugin_probe_target.clone();
                        self.refresh_plugins(desired.as_deref());
                    }
                }
                JobTerminal::Success(AppJobOutput::PluginInstalled(receipt)) => {
                    if let Some(operation) = self.plugin_operation.lock().unwrap().as_mut() {
                        operation.plugin = receipt.id.clone();
                    }
                    self.finish_plugin_operation(
                        PluginOperationState::Succeeded,
                        format!(
                            "installed {}@{} · {}",
                            receipt.id, receipt.version, receipt.digest
                        ),
                        None,
                    );
                    notices.push(BackgroundNotice::info(
                        result.kind,
                        format!("installed {}@{}", receipt.id, receipt.version),
                    ));
                    let desired = self.plugin_probe_target.clone();
                    self.refresh_plugins(desired.as_deref());
                }
                JobTerminal::Success(AppJobOutput::PluginLifecycle(lifecycle)) => {
                    let phases = if lifecycle.phases.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", lifecycle.phases.join(" → "))
                    };
                    notices.push(BackgroundNotice::info(
                        result.kind,
                        format!(
                            "{} {} complete{}: {}",
                            lifecycle.plugin, lifecycle.operation, phases, lifecycle.result
                        ),
                    ));
                    self.finish_plugin_operation(
                        PluginOperationState::Succeeded,
                        lifecycle.result.to_string(),
                        None,
                    );
                    let desired = self.plugin_probe_target.clone();
                    self.refresh_plugins(desired.as_deref());
                }
                JobTerminal::Success(AppJobOutput::SourceRevisions(revs)) => {
                    self.apply_source_revisions(&result.scope.project, revs);
                    self.source_revs_loading = false;
                    self.source_revs_error = None;
                }
                JobTerminal::Success(AppJobOutput::OpencodeModels(models)) => {
                    self.opencode_models = ModelDiscovery::Ready(models);
                }
                JobTerminal::Success(AppJobOutput::LaunchReady(ready)) => {
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        if let Err(error) = self.apply_launch_ready(run_id, ready) {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                &error.to_string(),
                            );
                            notices.push(BackgroundNotice::error(result.kind, error.to_string()));
                        }
                    }
                }
                JobTerminal::Success(AppJobOutput::CorpusSnapshot(snapshot)) => {
                    let project = result.scope.project;
                    self.corpus_stats = Some(snapshot.stats);
                    self.mission_logs = snapshot.logs;
                    if let Some((cost, cache)) = snapshot.cost {
                        self.corpus_cost = Some(cost);
                        self.corpus_cost_cache = cache;
                    }
                    self.apply_findings(project.as_str(), snapshot.findings);
                    self.corpus_stats_project = Some(project);
                    self.corpus_polled_at = Some(self.clock.monotonic_now());
                }
                JobTerminal::Success(AppJobOutput::LiveSessions(sessions)) => {
                    self.apply_live_sessions(sessions);
                }
                JobTerminal::Success(AppJobOutput::SessionMaintenance(maintenance)) => {
                    let project = result.scope.project;
                    if let Some(warning) = maintenance.warning {
                        notices.push(BackgroundNotice::info(result.kind, warning));
                    }
                    let mut missions_changed = false;
                    for (slug, tmux, conversation) in maintenance.conversations {
                        let lease = self.session_operation_leases.claim(&project, &slug);
                        let Ok(_ownership) = lease.try_lock() else {
                            // Teardown owns this mission. Discovery is
                            // optional bookkeeping and must never stall the
                            // UI or write across that destructive boundary.
                            continue;
                        };
                        // The project generation guard is not enough: a
                        // mission can stop and relaunch within one project
                        // generation. Bind only if this is still the exact
                        // launch the worker inspected.
                        let durable_launch_is_current =
                            load_launchable_mission(&self.store, &project, &slug).is_ok_and(
                                |mission| {
                                    mission.session.as_deref() == Some(tmux.as_str())
                                        && mission.opencode_session.is_none()
                                },
                            );
                        let launch_is_current = durable_launch_is_current
                            && self
                                .trees
                                .get(&project)
                                .into_iter()
                                .flat_map(|tree| tree.missions.iter())
                                .find(|(candidate, _)| candidate == &slug)
                                .is_some_and(|(_, mission)| {
                                    mission.session.as_deref() == Some(tmux.as_str())
                                        && mission.opencode_session.is_none()
                                        && mission.delete_requested.is_none()
                                });
                        if !launch_is_current {
                            continue;
                        }
                        if self
                            .set_opencode_session(&project, &slug, Some(conversation))
                            .is_ok()
                        {
                            missions_changed = true;
                        }
                    }
                    let exported = !maintenance.exported_tmux.is_empty();
                    for tmux in maintenance.exported_tmux {
                        self.export_retry_after.remove(&tmux);
                        self.last_exported_at
                            .insert(tmux, self.clock.monotonic_now());
                    }
                    if let Some((tmux, error)) = maintenance.export_failure {
                        self.export_retry_after
                            .insert(tmux, self.clock.monotonic_now() + Duration::from_secs(30));
                        notices.push(BackgroundNotice::error(
                            result.kind,
                            format!("usage checkpoint failed: {error}"),
                        ));
                    }
                    if missions_changed {
                        self.refresh_missions(&project);
                    }
                    if exported {
                        self.note_corpus_mutation(&project);
                        self.refresh_corpus_stats(&project);
                    }
                    self.schedule_dispatch_deliveries();
                }
                JobTerminal::Success(AppJobOutput::DispatchDeliveries) => {}
                JobTerminal::Success(AppJobOutput::TeardownReady(ready)) => {
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        let (is_error, message) = self.apply_teardown_ready(run_id, ready);
                        notices.push(if is_error {
                            BackgroundNotice::error(result.kind, message)
                        } else {
                            BackgroundNotice::info(result.kind, message)
                        });
                    }
                }
                JobTerminal::Success(AppJobOutput::OrphanCleanup { project, plugin }) => {
                    if self.effective_project().as_deref() == Some(project.as_str()) {
                        self.refresh_plugins(Some(&plugin));
                    }
                }
                JobTerminal::Success(AppJobOutput::ProjectScope(snapshot)) => {
                    let project = result.scope.project;
                    self.agents = snapshot.agents;
                    self.agents_project = Some(project.clone());
                    self.missions = snapshot.missions;
                    self.missions_project = Some(project.clone());
                    if let Some(tree) = self.trees.get_mut(&project) {
                        tree.agents = self.agents.clone();
                        tree.missions = self.missions.clone();
                    }
                    self.corpus_stats = Some(snapshot.stats);
                    self.mission_logs = snapshot.logs;
                    self.apply_findings(project.as_str(), snapshot.findings);
                    self.corpus_stats_project = Some(project);
                    self.corpus_polled_at = Some(self.clock.monotonic_now());
                }
                JobTerminal::Success(AppJobOutput::LaunchRequests {
                    launches,
                    deletions,
                    agent_deletions,
                    project_deletions,
                }) => {
                    self.apply_mission_requests(
                        deletions,
                        agent_deletions,
                        project_deletions,
                        launches,
                    );
                }
                JobTerminal::Success(AppJobOutput::ProjectIndex(projects, trees)) => {
                    self.projects = projects;
                    self.trees = trees;
                }
                JobTerminal::Success(AppJobOutput::Agents(agents)) => {
                    let project = result.scope.project;
                    self.agents = agents;
                    self.agents_project = Some(project.clone());
                    if let Some(tree) = self.trees.get_mut(&project) {
                        tree.agents = self.agents.clone();
                    }
                }
                JobTerminal::Success(AppJobOutput::Missions(missions)) => {
                    let project = result.scope.project;
                    self.missions = missions;
                    self.missions_project = Some(project.clone());
                    if let Some(tree) = self.trees.get_mut(&project) {
                        tree.missions = self.missions.clone();
                    }
                }
                JobTerminal::Failure(error) => {
                    if self.retry_stale_plugin_probe(result.kind) {
                        continue;
                    }
                    self.mark_job_failed(result.kind, &result.scope, &error);
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        if result.kind == JobKind::LaunchPreparation {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                &error,
                            );
                        }
                        let wrapped = Error::Store(error.clone());
                        let teardown = result.kind == JobKind::SessionTeardown;
                        self.fail_run(
                            run_id,
                            if teardown {
                                RunPhaseKind::Stopping
                            } else {
                                RunPhaseKind::Preparing
                            },
                            &wrapped,
                            true,
                            teardown,
                        );
                    }
                    notices.push(BackgroundNotice::error(result.kind, error));
                }
                JobTerminal::Cancelled => {
                    if self.retry_stale_plugin_probe(result.kind) {
                        continue;
                    }
                    self.mark_job_failed(result.kind, &result.scope, "cancelled");
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        if result.kind == JobKind::LaunchPreparation {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                "launch cancelled",
                            );
                        }
                        self.finish_run(run_id);
                    }
                    notices.push(BackgroundNotice::info(
                        result.kind,
                        "background work cancelled",
                    ));
                }
                JobTerminal::TimedOut => {
                    if self.retry_stale_plugin_probe(result.kind) {
                        continue;
                    }
                    self.mark_job_failed(result.kind, &result.scope, "timed out");
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        let error = Error::Store(format!("{} timed out", result.kind.label()));
                        if result.kind == JobKind::LaunchPreparation {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                &error.to_string(),
                            );
                        }
                        let teardown = result.kind == JobKind::SessionTeardown;
                        self.fail_run(
                            run_id,
                            if teardown {
                                RunPhaseKind::Stopping
                            } else {
                                RunPhaseKind::Preparing
                            },
                            &error,
                            true,
                            teardown,
                        );
                    }
                    notices.push(BackgroundNotice::error(
                        result.kind,
                        format!("{} timed out", result.kind.label()),
                    ));
                }
            }
        }
        notices
    }

    fn mark_job_failed(&mut self, kind: JobKind, scope: &JobScope, error: &str) {
        match kind {
            JobKind::PluginProbe => {
                self.plugin_probe_active = None;
                self.plugin_probe_active_project = None;
                self.plugins_loading = false;
                self.plugins_error = Some(error.to_string());
            }
            JobKind::PluginInstall
            | JobKind::PluginSetup
            | JobKind::PluginDoctor
            | JobKind::PluginStop => {
                let state = if error == "cancelled" {
                    PluginOperationState::Cancelled
                } else {
                    PluginOperationState::Failed
                };
                self.finish_plugin_operation(
                    state,
                    error.to_string(),
                    plugin_recovery_hint(error).map(str::to_string),
                );
            }
            JobKind::SourceRevisions => {
                // Keep the previous revision list/pins; only the refresh failed.
                self.source_revs_loading = false;
                self.source_revs_error = Some(error.to_string());
            }
            JobKind::ModelDiscovery => {
                self.opencode_models = ModelDiscovery::Failed(error.to_string());
            }
            JobKind::ProjectScope | JobKind::CorpusSummary | JobKind::CorpusCost => {
                self.fail_findings(&scope.project, error);
            }
            _ => {}
        }
    }

    pub(super) fn apply_source_revisions(&mut self, project: &str, revs: Vec<SourceRevs>) {
        if !revs.is_empty() {
            let stored: BTreeMap<String, String> = self
                .projects
                .iter()
                .find(|(slug, _)| slug == project)
                .map(|(_, project)| project.pins.clone())
                .unwrap_or_default();
            self.source_pins = revs
                .iter()
                .map(|source| {
                    let rev = stored
                        .get(&source.name)
                        .filter(|rev| source.revs.contains(rev))
                        .cloned()
                        .unwrap_or_else(|| source.default_rev().to_string());
                    (source.name.clone(), rev)
                })
                .collect();
        }
        self.source_revs = revs;
        self.source_revs_project = Some(project.to_string());
    }

    /// Whether a background result still belongs to current UI/run state.
    /// Non-run work follows the selected project; run work follows its stable
    /// generation even when the operator navigates elsewhere.
    pub fn job_scope_current(&self, scope: &crate::jobs::JobScope) -> bool {
        if scope.project.is_empty() {
            return scope.run_id.is_none();
        }
        let project_current = self.projects.iter().any(|(slug, project)| {
            slug == &scope.project && project.corpus_generation == scope.project_generation
        });
        if !project_current {
            return false;
        }
        match &scope.run_id {
            Some(run_id) => {
                run_id.project == scope.project
                    && self
                        .run_generations
                        .get(&(run_id.project.clone(), run_id.mission.clone()))
                        .is_some_and(|generation| *generation == run_id.generation)
            }
            None => self.effective_project().as_deref() == Some(scope.project.as_str()),
        }
    }
}

/// A selected-only probe returns unprobed catalog rows for every other
/// plugin. Preserve their last checked result rather than turning a healthy
/// cached badge back into "unknown" whenever another picker entry is probed.
pub(super) fn merge_plugin_statuses(
    previous: &[PluginStatus],
    discovered: Vec<PluginStatus>,
) -> Vec<PluginStatus> {
    discovered
        .into_iter()
        .map(|status| {
            if status.probed {
                status
            } else {
                previous
                    .iter()
                    .find(|old| old.name == status.name && old.probed)
                    .cloned()
                    .unwrap_or(status)
            }
        })
        .collect()
}
