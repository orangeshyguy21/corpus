//! Plugin discovery, lifecycle, and durable lease coordination.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use corpus_core::{Mission, PluginStatus, Store};

use super::{
    global_job_scope, mission_label, AppJobOutput, AppState, PluginLeaseView,
    PluginLifecycleResult, PluginOperationState, PluginOperationView,
};
use crate::jobs::{JobKind, JobScope, JobSet, StartOutcome};

impl AppState {
    pub(super) fn retry_stale_plugin_probe(&mut self, kind: JobKind) -> bool {
        if kind != JobKind::PluginProbe {
            return false;
        }
        let Some(finished) = self.plugin_probe_active.take() else {
            return false;
        };
        let finished_project = self.plugin_probe_active_project.take().flatten();
        if finished == self.plugin_probe_target && finished_project == self.plugin_probe_project {
            return false;
        }
        let desired = self.plugin_probe_target.clone();
        self.refresh_plugins(desired.as_deref());
        true
    }

    pub(super) fn finish_plugin_operation(
        &self,
        state: PluginOperationState,
        detail: String,
        recovery: Option<String>,
    ) {
        let mut operation = self.plugin_operation.lock().unwrap();
        if let Some(current) = operation.as_mut() {
            current.state = state;
            current.detail = detail;
            current.recovery = recovery;
        }
    }

    /// Discover every plugin manifest but live-probe only `selected`.
    /// Host process work stays in corpus-core and on the P1 job boundary.
    pub fn refresh_plugins(&mut self, selected: Option<&str>) {
        let target = selected.map(str::to_string);
        let project = self.effective_project();
        self.plugin_probe_target = target.clone();
        self.plugin_probe_project = project.clone();
        self.plugins_loading = true;
        self.plugins_error = None;
        let Some(jobs) = self.jobs.as_mut() else {
            self.plugins = corpus_core::selected_plugin_status(target.as_deref());
            self.plugin_leases = prepared_plugin_leases(
                &self.store,
                project.as_deref(),
                target.as_deref(),
                &self.plugins,
            );
            self.plugins_loading = false;
            return;
        };
        let work_target = target.clone();
        let work_project = project.clone();
        let store = self.store.clone();
        let outcome = jobs.start(
            JobKind::PluginProbe,
            JobScope {
                project: String::new(),
                project_generation: 0,
                corpus_revision: None,
                run_id: None,
            },
            Duration::from_secs(30),
            move |_| {
                let statuses = corpus_core::selected_plugin_status(work_target.as_deref());
                Ok(AppJobOutput::Plugins {
                    leases: prepared_plugin_leases(
                        &store,
                        work_project.as_deref(),
                        work_target.as_deref(),
                        &statuses,
                    ),
                    statuses,
                    target: work_target,
                    project: work_project,
                })
            },
        );
        if matches!(outcome, StartOutcome::Started(_)) {
            self.plugin_probe_active = Some(target);
            self.plugin_probe_active_project = Some(project);
        }
    }

    /// The last plugin probe results (empty until `refresh_plugins`).
    pub fn plugins(&self) -> &[PluginStatus] {
        &self.plugins
    }

    pub fn plugin_leases(&self) -> &[PluginLeaseView] {
        &self.plugin_leases
    }

    /// Close a durable lease whose mission record is already gone. The
    /// reconciliation beat invokes this automatically; the UI calls it only
    /// as an immediate retry after automatic cleanup failed.
    pub fn cleanup_orphan_environment(
        &mut self,
        plugin_id: &str,
        session_key: &str,
    ) -> Result<bool, String> {
        let record = self
            .store
            .load_environment_session_key(plugin_id, session_key)
            .map_err(|error| error.to_string())?;
        if self
            .store
            .load_mission(&record.id.project, &record.id.mission)
            .is_ok()
        {
            return Err(
                "environment still belongs to a mission; delete that mission instead".into(),
            );
        }
        if self.jobs.is_none() {
            corpus_core::close_environment_session_key(&self.store, plugin_id, session_key)
                .map_err(|error| error.to_string())?;
            self.refresh_plugins(Some(plugin_id));
            return Ok(true);
        }

        let store = self.store.clone();
        let plugin = plugin_id.to_string();
        let key = session_key.to_string();
        let project = record.id.project;
        let session_operation = self
            .session_operation_leases
            .claim(&project, &record.id.mission);
        let scope = self.job_scope(&project, None);
        let jobs = self.jobs.as_mut().expect("checked above");
        Ok(matches!(
            jobs.start(
                JobKind::OrphanCleanup,
                scope,
                Duration::from_secs(30),
                move |_| {
                    let _ownership = session_operation
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    corpus_core::close_environment_session_key(&store, &plugin, &key)
                        .map_err(|error| error.to_string())?;
                    Ok(AppJobOutput::OrphanCleanup { project, plugin })
                },
            ),
            StartOutcome::Started(_)
        ))
    }

    pub fn plugin_operation(&self) -> Option<PluginOperationView> {
        self.plugin_operation.lock().unwrap().clone()
    }

    /// Install and atomically select a local immutable v1 bundle without
    /// asking the operator to leave the app. Archive download remains a
    /// release/marketplace concern; Chunk 7 accepts an unpacked bundle path.
    pub(crate) fn start_plugin_install(&mut self, bundle: &str) -> Result<bool, String> {
        let bundle = bundle.trim();
        if bundle.is_empty() {
            return Err("choose an unpacked plugin bundle directory".into());
        }
        let Some(jobs) = self.jobs.as_mut() else {
            return Err("plugin installation requires the app background-job runtime".into());
        };
        if plugin_work_active(jobs) {
            return Ok(false);
        }
        let path = PathBuf::from(bundle);
        *self.plugin_operation.lock().unwrap() = Some(PluginOperationView {
            plugin: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("plugin bundle")
                .to_string(),
            operation: "install".into(),
            state: PluginOperationState::Running,
            phase: Some("validating bundle".into()),
            detail: path.display().to_string(),
            recovery: None,
        });
        Ok(matches!(
            jobs.start(
                JobKind::PluginInstall,
                global_job_scope(),
                Duration::from_secs(120),
                move |_| corpus_core::install_plugin_bundle(&path)
                    .map(AppJobOutput::PluginInstalled)
                    .map_err(|error| error.to_string()),
            ),
            StartOutcome::Started(_)
        ))
    }

    /// Download a Corpus-curated release, verify its pinned checksum and
    /// catalog identity, then hand it to the same immutable installer used by
    /// local bundles.
    pub(crate) fn start_curated_plugin_install(&mut self, id: &str) -> Result<bool, String> {
        let plugin = corpus_core::curated_plugin(id).map_err(|error| error.to_string())?;
        let Some(jobs) = self.jobs.as_mut() else {
            return Err("plugin installation requires the app background-job runtime".into());
        };
        if plugin_work_active(jobs) {
            return Ok(false);
        }

        let operation_state = self.plugin_operation.clone();
        *operation_state.lock().unwrap() = Some(PluginOperationView {
            plugin: plugin.id.clone(),
            operation: "install".into(),
            state: PluginOperationState::Running,
            phase: Some("downloading release".into()),
            detail: format!("{}@{}", plugin.id, plugin.version),
            recovery: None,
        });
        let plugin_id = plugin.id;
        Ok(matches!(
            jobs.start(
                JobKind::PluginInstall,
                global_job_scope(),
                Duration::from_secs(180),
                move |cancellation| {
                    corpus_core::install_curated_plugin_with(
                        &plugin_id,
                        || cancellation.is_cancelled(),
                        |phase| {
                            if let Some(current) = operation_state.lock().unwrap().as_mut() {
                                current.phase = Some(phase.label().into());
                            }
                        },
                    )
                    .map(AppJobOutput::PluginInstalled)
                    .map_err(|error| error.to_string())
                },
            ),
            StartOutcome::Started(_)
        ))
    }

    /// Start an installation-scoped plugin lifecycle operation. The worker
    /// resolves the selected bundle and spawns the executable off the render
    /// thread; cancellation is observed by the protocol client every 100ms.
    pub(crate) fn start_plugin_lifecycle(
        &mut self,
        plugin_id: &str,
        operation: &'static str,
    ) -> Result<bool, String> {
        let kind = match operation {
            "setup" => JobKind::PluginSetup,
            "doctor" | "status" => JobKind::PluginDoctor,
            "stop" => JobKind::PluginStop,
            _ => {
                return Err(format!(
                    "unsupported plugin lifecycle operation {operation:?}"
                ))
            }
        };
        let timeout = if operation == "setup" {
            Duration::from_secs(30 * 60)
        } else {
            Duration::from_secs(120)
        };
        let plugin_id = plugin_id.to_string();
        let Some(jobs) = self.jobs.as_mut() else {
            return Err("plugin lifecycle requires the app background-job runtime".into());
        };
        if plugin_work_active(jobs) {
            return Ok(false);
        }
        let operation_state = self.plugin_operation.clone();
        *operation_state.lock().unwrap() = Some(PluginOperationView {
            plugin: plugin_id.clone(),
            operation: operation.into(),
            state: PluginOperationState::Running,
            phase: Some(if operation == "setup" {
                "preparing sources".into()
            } else {
                format!("running {operation}")
            }),
            detail: String::new(),
            recovery: None,
        });
        Ok(matches!(
            jobs.start(kind, global_job_scope(), timeout, move |cancellation| {
                let selected = corpus_core::find_plugin(&plugin_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("plugin not found: {plugin_id}"))?;
                let mut phases = Vec::new();
                let result = corpus_core::call_plugin_lifecycle_cancellable(
                    &selected,
                    operation,
                    timeout.saturating_sub(Duration::from_secs(1)),
                    || cancellation.is_cancelled(),
                    |progress| {
                        phases.push(progress.phase.clone());
                        if let Some(current) = operation_state.lock().unwrap().as_mut() {
                            current.phase = Some(progress.phase.clone());
                            current.detail = match (progress.completed, progress.total) {
                                (Some(completed), Some(total)) => {
                                    format!("{} · {completed}/{total}", progress.message)
                                }
                                _ => progress.message.clone(),
                            };
                        }
                    },
                )
                .map_err(|error| error.to_string())?;
                Ok(AppJobOutput::PluginLifecycle(PluginLifecycleResult {
                    plugin: plugin_id,
                    operation,
                    phases,
                    result,
                }))
            }),
            StartOutcome::Started(_)
        ))
    }

    pub(crate) fn cancel_plugin_lifecycle(&self, operation: &str) -> bool {
        let kind = match operation {
            "setup" => JobKind::PluginSetup,
            "doctor" | "status" => JobKind::PluginDoctor,
            "stop" => JobKind::PluginStop,
            _ => return false,
        };
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.cancel_kind(kind) > 0)
    }

    pub(crate) fn plugin_lifecycle_active(&self, operation: &str) -> bool {
        let kind = match operation {
            "setup" => JobKind::PluginSetup,
            "doctor" | "status" => JobKind::PluginDoctor,
            "stop" => JobKind::PluginStop,
            _ => return false,
        };
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.is_kind_active(kind))
    }

    pub(crate) fn plugin_work_active(&self) -> bool {
        self.jobs.as_ref().is_some_and(plugin_work_active)
    }

    pub fn env_probe_loading(&self, project: &str) -> bool {
        self.plugins_loading
            && self.env_project.as_deref() == Some(project)
            && self
                .projects
                .iter()
                .find(|(slug, _)| slug == project)
                .is_some_and(|(_, spec)| {
                    self.plugin_probe_target.as_deref() == Some(spec.plugin.as_str())
                })
    }

    pub fn env_probe_error(&self, project: &str) -> Option<&str> {
        let targets_binding = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)
            .is_some_and(|(_, spec)| {
                self.plugin_probe_target.as_deref() == Some(spec.plugin.as_str())
            });
        if self.env_project.as_deref() == Some(project) && targets_binding {
            self.plugins_error.as_deref()
        } else {
            None
        }
    }
}

fn plugin_work_active(jobs: &JobSet<AppJobOutput>) -> bool {
    [
        JobKind::PluginInstall,
        JobKind::PluginSetup,
        JobKind::PluginDoctor,
        JobKind::PluginStop,
    ]
    .into_iter()
    .any(|kind| jobs.is_kind_active(kind))
}

/// Load durable leases and evaluate only drift that can be proven without a
/// fetch: immutable plugin identity, manifest-default pins, and literal SHA
/// pins. Named custom revs remain visible as recorded identities but are not
/// guessed from ambient network state.
pub(super) fn prepared_plugin_leases(
    store: &Store,
    project: Option<&str>,
    plugin_id: Option<&str>,
    statuses: &[PluginStatus],
) -> Vec<PluginLeaseView> {
    let (Some(project), Some(plugin_id)) = (project, plugin_id) else {
        return Vec::new();
    };
    let selected = statuses.iter().find(|status| status.name == plugin_id);
    let manifest = corpus_core::find_plugin(plugin_id).ok().flatten();
    let mut leases = Vec::new();
    let missions: BTreeMap<String, Mission> = store
        .list_missions(project)
        .unwrap_or_default()
        .into_iter()
        .collect();
    for record in store
        .list_environment_sessions(plugin_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|record| record.id.project == project)
    {
        if record.state == corpus_core::EnvironmentSessionState::Closed {
            continue;
        }
        let mission_slug = record.id.mission.clone();
        let mission = missions.get(&mission_slug);
        let mut drift = Vec::new();
        if mission.is_none() {
            drift.push("mission record is missing; automatic orphan cleanup pending".into());
        }
        if record.plugin_id != plugin_id {
            drift.push(format!(
                "plugin id {} != selected {plugin_id}",
                record.plugin_id
            ));
        }
        if let Some(status) = selected {
            if status.version.as_deref() != Some(record.plugin_version.as_str()) {
                drift.push(format!(
                    "plugin version {} != selected {}",
                    record.plugin_version,
                    status.version.as_deref().unwrap_or("unknown")
                ));
            }
            if status.bundle_digest.as_deref() != Some(record.plugin_digest.as_str()) {
                drift.push("plugin bundle digest differs from selected install".into());
            }
            if let (Some(prepared), Some(lease)) = (
                status.prepared.environment_lock.as_deref(),
                record.environment_lock.as_deref(),
            ) {
                if prepared != lease {
                    drift.push("environment lock differs from prepared environment".into());
                }
            }
        }
        if let (Some(plugin), Some(mission)) = (manifest.as_ref(), mission) {
            for source in &plugin.manifest.sources {
                let chosen = mission
                    .pins
                    .get(&source.id)
                    .map(String::as_str)
                    .unwrap_or(source.default_rev.as_str());
                let expected = if corpus_core::is_commit_sha(chosen) {
                    Some(chosen)
                } else if chosen == source.default_rev {
                    Some(source.default_sha.as_str())
                } else {
                    None
                };
                if let (Some(expected), Some(active)) =
                    (expected, record.source_shas.get(&source.id))
                {
                    if expected != active {
                        drift.push(format!(
                            "{} pin resolves to {} but lease runs {}",
                            source.id, expected, active
                        ));
                    }
                }
            }
        }
        leases.push(PluginLeaseView {
            session_key: record.id.storage_key(),
            mission: mission_label(
                mission.and_then(|record| record.name.as_deref()),
                &mission_slug,
            ),
            mission_slug,
            orphaned: mission.is_none(),
            state: record.state,
            plugin_version: record.plugin_version,
            plugin_digest: record.plugin_digest,
            source_shas: record.source_shas,
            environment_lock: record.environment_lock,
            image_digest: record.image_digest,
            drift,
            error: record.error,
        });
    }
    leases.sort_by(|a, b| (&a.mission, &a.mission_slug).cmp(&(&b.mission, &b.mission_slug)));
    leases
}

pub(super) fn orphan_environment_sessions(store: &Store) -> Vec<(String, String)> {
    store
        .list_all_environment_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|record| {
            !matches!(
                record.state,
                corpus_core::EnvironmentSessionState::Closed
                    | corpus_core::EnvironmentSessionState::Failed
            )
        })
        .filter(|record| {
            store
                .load_mission(&record.id.project, &record.id.mission)
                .is_err()
        })
        .map(|record| (record.plugin_id, record.id.storage_key()))
        .collect()
}

pub(super) fn plugin_recovery_hint(error: &str) -> Option<&'static str> {
    let error = error.to_ascii_lowercase();
    if error.contains("sessions_active") || error.contains("session(s) are active") {
        Some("Delete every live mission lease, then retry environment Stop.")
    } else if error.contains("source_missing")
        || error.contains("source identity mismatch")
        || error.contains("target identity")
        || error.contains("environment lock")
    {
        Some("Review the project source pins and selected plugin version, then run Setup and Doctor again.")
    } else if error.contains("isolation") || error.contains("cross_session") {
        Some("Stop the affected mission lease; retry only after Doctor confirms isolation.")
    } else if error.contains("cleanup") || error.contains("could not stop") {
        Some("Retry Stop. If a mission lease remains, delete that mission to retry its cleanup.")
    } else if error.contains("docker") {
        Some("Start Docker and retry Setup or Doctor.")
    } else if error.contains("immutable") || error.contains("already installed with digest") {
        Some("Install a new plugin version; immutable installed versions cannot be overwritten.")
    } else {
        None
    }
}
