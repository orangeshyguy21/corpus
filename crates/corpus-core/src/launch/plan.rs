//! Immutable identity and inputs for one runner launch.

use std::path::{Path, PathBuf};

use corpus_store::EnvironmentSessionId;

/// Execution shape selected before process construction begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchMode {
    /// Prefer the detached TUI, falling back to the piped backend when tmux is
    /// unavailable.
    Interactive,
    /// Re-open one existing OpenCode conversation in a detached TUI.
    Resume { session_id: String },
    /// Always run the piped automation backend, optionally appending to an
    /// existing transcript.
    Headless { append_to: Option<PathBuf> },
}

/// Complete immutable launch intent shared by every backend.
///
/// The compatibility entry points resolve their legacy positional arguments
/// into this owned value before filesystem or process work starts. Backends
/// may inspect the plan but cannot rewrite launch identity midway through a
/// spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchPlan {
    project: String,
    agent: String,
    model: Option<String>,
    mission: String,
    source_pins_json: Option<String>,
    environment_session: Option<String>,
    run_id: Option<EnvironmentSessionId>,
    mode: LaunchMode,
}

impl LaunchPlan {
    pub(crate) fn interactive(
        project: &str,
        agent: &str,
        model: String,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        run_id: Option<&EnvironmentSessionId>,
    ) -> Self {
        Self::new(
            project,
            agent,
            Some(model),
            mission,
            source_pins_json,
            environment_session,
            run_id,
            LaunchMode::Interactive,
        )
    }

    pub(crate) fn resume(
        project: &str,
        agent: &str,
        model: String,
        session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        run_id: Option<&EnvironmentSessionId>,
    ) -> Self {
        Self::new(
            project,
            agent,
            Some(model),
            "",
            source_pins_json,
            environment_session,
            run_id,
            LaunchMode::Resume {
                session_id: session_id.to_string(),
            },
        )
    }

    pub(crate) fn headless(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        append_to: Option<&Path>,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
    ) -> Self {
        Self::new(
            project,
            agent,
            model.map(str::to_string),
            mission,
            source_pins_json,
            environment_session,
            None,
            LaunchMode::Headless {
                append_to: append_to.map(Path::to_path_buf),
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        project: &str,
        agent: &str,
        model: Option<String>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        run_id: Option<&EnvironmentSessionId>,
        mode: LaunchMode,
    ) -> Self {
        debug_assert!(
            run_id.is_none_or(|identity| identity.project == project),
            "run identity must belong to the launch project"
        );
        Self {
            project: project.to_string(),
            agent: agent.to_string(),
            model,
            mission: mission.to_string(),
            source_pins_json: source_pins_json.map(str::to_string),
            environment_session: environment_session.map(str::to_string),
            run_id: run_id.cloned(),
            mode,
        }
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    pub(crate) fn agent(&self) -> &str {
        &self.agent
    }

    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub(crate) fn mission(&self) -> &str {
        &self.mission
    }

    pub(crate) fn source_pins_json(&self) -> Option<&str> {
        self.source_pins_json.as_deref()
    }

    pub(crate) fn environment_session(&self) -> Option<&str> {
        self.environment_session.as_deref()
    }

    pub(crate) fn run_id(&self) -> Option<&EnvironmentSessionId> {
        self.run_id.as_ref()
    }

    pub(crate) fn mode(&self) -> &LaunchMode {
        &self.mode
    }

    pub(crate) fn resume_session(&self) -> Option<&str> {
        match &self.mode {
            LaunchMode::Resume { session_id } => Some(session_id),
            LaunchMode::Interactive | LaunchMode::Headless { .. } => None,
        }
    }

    pub(crate) fn append_to(&self) -> Option<&Path> {
        match &self.mode {
            LaunchMode::Headless { append_to } => append_to.as_deref(),
            LaunchMode::Interactive | LaunchMode::Resume { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_owns_every_launch_identity_input() {
        let mut project = "alpha".to_string();
        let mut prompt = "investigate".to_string();
        let run_id = EnvironmentSessionId {
            project: project.clone(),
            mission: "curator-campaign".into(),
            generation: 3,
        };
        let plan = LaunchPlan::interactive(
            &project,
            "runner",
            "mlx/qwen3.8".into(),
            &prompt,
            Some("{\"repo\":\"sha\"}"),
            Some("environment-key"),
            Some(&run_id),
        );
        project.clear();
        prompt.clear();

        assert_eq!(plan.project(), "alpha");
        assert_eq!(plan.agent(), "runner");
        assert_eq!(plan.model(), Some("mlx/qwen3.8"));
        assert_eq!(plan.mission(), "investigate");
        assert_eq!(plan.run_id(), Some(&run_id));
        assert_eq!(plan.mode(), &LaunchMode::Interactive);
    }

    #[test]
    fn backend_specific_inputs_cannot_overlap() {
        let resume = LaunchPlan::resume("p", "a", "m".into(), "ses_1", None, None, None);
        assert_eq!(resume.resume_session(), Some("ses_1"));
        assert_eq!(resume.append_to(), None);

        let transcript = Path::new("runs/existing.log");
        let headless = LaunchPlan::headless("p", "a", None, "prompt", Some(transcript), None, None);
        assert_eq!(headless.resume_session(), None);
        assert_eq!(headless.append_to(), Some(transcript));
    }
}
