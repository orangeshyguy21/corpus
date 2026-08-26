//! Render-safe application state and projections.
//!
//! These types contain no filesystem, process, session, or job orchestration.

use std::collections::BTreeMap;

use corpus_core::{AgentConfig, FindingCard, Mission};

use crate::jobs::JobKind;

/// One project's subtree in the sidebar tree.
#[derive(Debug, Clone, Default)]
pub struct ProjectTree {
    pub agents: Vec<(String, AgentConfig)>,
    pub missions: Vec<(String, Mission)>,
}

/// Lightweight counters for confirming that the cheap tmux liveness beat is
/// no longer coupled one-for-one to expensive session reconciliation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionLifecycleStats {
    pub live_refreshes: u64,
    pub reconciliation_passes: u64,
}

/// A durable environment lease projected for the selected project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLeaseView {
    pub session_key: String,
    pub mission: String,
    pub mission_slug: String,
    pub orphaned: bool,
    pub state: corpus_core::EnvironmentSessionState,
    pub plugin_version: String,
    pub plugin_digest: String,
    pub source_shas: BTreeMap<String, String>,
    pub environment_lock: Option<String>,
    pub image_digest: Option<String>,
    pub drift: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOperationState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Retained operator-facing lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginOperationView {
    pub plugin: String,
    pub operation: String,
    pub state: PluginOperationState,
    pub phase: Option<String>,
    pub detail: String,
    pub recovery: Option<String>,
}

/// A completed background job's operator-facing notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundNotice {
    pub severity: BackgroundNoticeSeverity,
    pub job_kind: JobKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundNoticeSeverity {
    Info,
    Error,
    Resolved,
}

impl BackgroundNotice {
    pub(crate) fn info(job_kind: JobKind, message: impl Into<String>) -> Self {
        Self {
            severity: BackgroundNoticeSeverity::Info,
            job_kind,
            message: message.into(),
        }
    }

    pub(crate) fn error(job_kind: JobKind, message: impl Into<String>) -> Self {
        Self {
            severity: BackgroundNoticeSeverity::Error,
            job_kind,
            message: message.into(),
        }
    }

    pub(crate) fn resolved(job_kind: JobKind) -> Self {
        Self {
            severity: BackgroundNoticeSeverity::Resolved,
            job_kind,
            message: String::new(),
        }
    }
}

/// A curator-requested launch result queued for one operator-facing toast.
#[derive(Debug, Clone)]
pub struct LaunchNotice {
    pub mission: String,
    pub result: Result<(), String>,
}

/// The environment probe projection consumed by the top bar.
#[derive(Debug, Clone)]
pub struct EnvStatus {
    pub name: String,
    pub ready: bool,
    pub notes: String,
    pub running_version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub enum ModelDiscovery {
    #[default]
    Loading,
    Ready(corpus_core::ModelList),
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub enum FindingDiscovery {
    #[default]
    Loading,
    Ready(Vec<FindingCard>),
    Failed {
        message: String,
        last_good: Vec<FindingCard>,
    },
}

#[derive(Debug)]
pub enum StopMissionResult {
    Scheduled,
    Completed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMissionResult {
    Scheduled,
    Completed,
}

/// Whether a project disappeared immediately or remains durably marked for
/// deletion while child mission teardown finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteProjectResult {
    Scheduled,
    Completed,
}

/// The compact operator-facing state used by mission lists and status dots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionDisplayState {
    Idle,
    Queued,
    Preparing,
    Starting,
    Working,
    Waiting,
    Stopping,
    Exporting,
    Failed,
    Deleting,
}

impl MissionDisplayState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Starting => "starting",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Stopping => "stopping",
            Self::Exporting => "exporting",
            Self::Failed => "failed",
            Self::Deleting => "deleting",
        }
    }
}

/// Who the active (or last-finished) run was.
#[derive(Debug, Clone)]
pub struct RunMeta {
    pub pty_attach: Option<Vec<String>>,
}

pub type RunId = corpus_core::EnvironmentSessionId;

/// A run that ended on its own, queued for one operator report.
#[derive(Debug, Clone)]
pub struct RunExit {
    pub mission: Option<String>,
    pub code: i32,
}

/// A run operation's authoritative application lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunPhase {
    Idle,
    Preparing,
    Starting,
    Running,
    Stopping,
    Exporting,
    Failed {
        at: RunPhaseKind,
        message: String,
        recoverable: bool,
        cleanup_pending: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhaseKind {
    Preparing,
    Starting,
    Running,
    Stopping,
    Exporting,
}
