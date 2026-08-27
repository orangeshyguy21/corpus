//! Typed command-line contract and generated top-level help.

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(
    name = "corpus",
    about = "Local-first vulnerability research platform",
    long_about = "Corpus is the headless command-line surface for local vulnerability research, environment-plugin diagnostics, and corpus-store administration."
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum CliCommand {
    /// Run an agent mission in the CORPUS_PROJECT scope.
    Run(RunArgs),
    /// Discover, install, select, diagnose, or call environment plugins.
    Plugin(PluginArgs),
    /// Inspect the benchmark model registry.
    Models(ModelsArgs),
    /// Manage projects in the corpus store.
    Project(ProjectArgs),
    /// Manage project agents and roles.
    Agent(AgentArgs),
    /// Manage project missions.
    Mission(MissionArgs),
    /// Discover or read projected findings.
    Finding(FindingArgs),
    /// Read a project's append-only curator audit log.
    Audit(AuditArgs),
    /// Read calls refused by identity, role, scope, or environment gates.
    Refusals(RefusalsArgs),
    /// Preserve the legacy unknown-command error while command domains migrate.
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct RunArgs {
    /// Agent slug within CORPUS_PROJECT.
    pub(crate) agent: String,
    /// Explicit provider/model identifier for both mission passes.
    #[arg(short = 'm', long)]
    pub(crate) model: Option<String>,
    /// Follow the mission with a researcher curation pass.
    #[arg(long)]
    pub(crate) research: bool,
    /// Mission prompt. Multiple words are joined with spaces.
    #[arg(required = true, num_args = 1..)]
    pub(crate) mission: Vec<String>,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct PluginArgs {
    #[command(subcommand)]
    pub(crate) command: PluginCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum PluginCommand {
    /// List discovered environment plugins.
    List,
    /// Download, verify, install, and select a supported plugin.
    Install {
        /// Plugin id from Corpus's built-in catalog.
        id: String,
    },
    /// Install an unpacked local bundle for plugin development.
    InstallLocal {
        /// Unpacked plugin bundle directory.
        bundle_dir: PathBuf,
    },
    /// Select an installed plugin version for upgrade or rollback.
    Select { id: String, version: String },
    /// Prepare sources, tools, images, and shared resources.
    Setup { id: String },
    /// Run the plugin's complete readiness diagnostics.
    Doctor { id: String },
    /// Read current plugin lifecycle status.
    Status { id: String },
    /// Stop the plugin's shared resources.
    Stop { id: String },
    /// Probe one plugin's environment health.
    Probe { name: String },
    /// Make a raw plugin protocol call for diagnostics.
    Call {
        name: String,
        method: String,
        /// Optional JSON method parameters.
        params_json: Option<String>,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ModelsArgs {
    #[command(subcommand)]
    pub(crate) command: ModelsCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum ModelsCommand {
    /// List the model registry used for benchmark metadata.
    List,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ProjectArgs {
    #[command(subcommand)]
    pub(crate) command: ProjectCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum ProjectCommand {
    /// List projects in the corpus store.
    List,
    /// Create an empty project.
    New {
        slug: String,
        #[arg(long)]
        name: Option<String>,
        /// Environment plugin binding.
        #[arg(long, default_value = "cdk-regtest")]
        plugin: String,
    },
    /// Clone project configuration, agents, and missions.
    Clone {
        slug: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        with_corpus: bool,
        #[arg(long)]
        name: Option<String>,
    },
    /// Delete a project or request lifecycle teardown for live missions.
    Delete { slug: String },
    /// Remove corpus contents while preserving the project and agents.
    Wipe { slug: String },
    /// Change a project's environment-plugin binding.
    Rebind {
        slug: String,
        #[arg(long)]
        plugin: String,
    },
    /// Migrate legacy attacks/ artifacts to the probes/ namespace.
    MigrateProbes {
        project: String,
        /// Apply the migration; omission is a dry run.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct AgentArgs {
    #[command(subcommand)]
    pub(crate) command: AgentCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum AgentCommand {
    /// List a project's agents.
    List { project: String },
    /// Create an agent with a role-defined capability ceiling.
    New {
        project: String,
        slug: String,
        #[arg(long, default_value = "researcher", value_parser = parse_agent_role)]
        role: corpus_core::AgentRole,
    },
    /// Clone an agent within one project.
    Clone {
        project: String,
        from: String,
        #[arg(long)]
        to: String,
    },
    /// Delete an agent or request teardown for its live missions.
    Delete { project: String, slug: String },
    /// Show or set an agent's role.
    Role {
        project: String,
        slug: String,
        #[arg(value_parser = parse_agent_role)]
        role: Option<corpus_core::AgentRole>,
    },
    /// Infer roles for agents created before explicit role metadata.
    MigrateRoles {
        project: String,
        /// Persist inferred roles; omission is a dry run.
        #[arg(long)]
        apply: bool,
    },
}

fn parse_agent_role(raw: &str) -> Result<corpus_core::AgentRole, String> {
    corpus_core::AgentRole::parse(raw).ok_or_else(|| {
        format!(
            "unknown role {raw:?}; expected one of {}",
            corpus_core::AgentRole::names()
        )
    })
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct MissionArgs {
    #[command(subcommand)]
    pub(crate) command: MissionCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum MissionCommand {
    /// List a project's missions.
    List { project: String },
    /// Create a mission and stamp its effective source revisions.
    New {
        project: String,
        slug: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        budget: Option<String>,
        /// Per-mission source override in source=revision form. Repeatable.
        #[arg(long = "pin", value_parser = parse_source_pin)]
        pins: Vec<SourcePin>,
        /// Mission brief. Multiple words are joined with spaces.
        #[arg(required = true, num_args = 1..)]
        brief: Vec<String>,
    },
    /// Delete a mission or request lifecycle teardown for a live run.
    Delete { project: String, slug: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourcePin {
    pub(crate) source: String,
    pub(crate) revision: String,
}

fn parse_source_pin(raw: &str) -> Result<SourcePin, String> {
    let Some((source, revision)) = raw.split_once('=') else {
        return Err("expected source=revision".to_string());
    };
    Ok(SourcePin {
        source: source.to_string(),
        revision: revision.to_string(),
    })
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct FindingArgs {
    #[command(subcommand)]
    pub(crate) command: FindingCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum FindingCommand {
    /// List projected findings with optional in-memory filters.
    List {
        project: String,
        /// Rated severities to include. Repeat or separate values with commas.
        #[arg(long = "severity", value_delimiter = ',')]
        severities: Vec<corpus_core::FindingSeverity>,
        /// Omit findings without a recognized severity.
        #[arg(long)]
        exclude_unrated: bool,
        /// Case-insensitive text query over projected finding fields.
        #[arg(long)]
        text: Option<String>,
        #[arg(long, default_value = "newest", value_parser = parse_finding_sort)]
        sort: corpus_core::FindingSort,
        #[arg(long, value_parser = parse_positive_usize)]
        limit: Option<usize>,
    },
    /// Print one Markdown finding exactly as stored.
    Show { project: String, path: String },
}

fn parse_finding_sort(raw: &str) -> Result<corpus_core::FindingSort, String> {
    match raw {
        "newest" => Ok(corpus_core::FindingSort::Newest),
        "severity" => Ok(corpus_core::FindingSort::Severity),
        _ => Err(format!(
            "invalid finding sort {raw:?}; expected newest or severity"
        )),
    }
}

fn parse_positive_usize(raw: &str) -> Result<usize, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|error| format!("invalid positive integer: {error}"))?;
    if value == 0 {
        return Err("value must be positive".to_string());
    }
    Ok(value)
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct AuditArgs {
    pub(crate) project: String,
    /// Number of most-recent records to print.
    #[arg(long, default_value_t = 50)]
    pub(crate) tail: usize,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct RefusalsArgs {
    pub(crate) project: String,
    /// Number of most-recent records to inspect before filtering.
    #[arg(long, default_value_t = 50)]
    pub(crate) tail: usize,
    /// Restrict output to one server gate.
    #[arg(long, value_parser = parse_refusal_gate)]
    pub(crate) gate: Option<corpus_core::refusal::Gate>,
}

fn parse_refusal_gate(raw: &str) -> Result<corpus_core::refusal::Gate, String> {
    use corpus_core::refusal::Gate;

    match raw {
        "identity" => Ok(Gate::Identity),
        "role" => Ok(Gate::Role),
        "scope" => Ok(Gate::Scope),
        "probe" => Ok(Gate::Probe),
        "args" => Ok(Gate::Args),
        "unknown" => Ok(Gate::Unknown),
        "harness" => Ok(Gate::Harness),
        _ => Err(format!(
            "invalid refusal gate {raw:?}; expected identity, role, scope, probe, args, unknown, or harness"
        )),
    }
}

fn command() -> clap::Command {
    let roles = corpus_core::AgentRole::names();
    Cli::command().after_long_help(format!(
        "\
Selected command details:
  run <agent> [-m <model>] [--research] <mission>...
      Materializes project agents, runs a headless mission, and records its
      transcript under the project corpus runs/ directory.
  agent new ... --role <role>
      Available roles: {roles}

Environment:
  CORPUS_HOME          Data root (default: ~/.corpus)
  CORPUS_STORE         Store root (default: <CORPUS_HOME>/store)
  CORPUS_RESOURCES     Shipped resource root
  CORPUS_PLUGINS_DIR   Development/test plugin catalog override
  CORPUS_SOURCES_DIR   Pinned-source cache override
  CORPUS_MODELS        Model registry override
  CORPUS_PROJECT       Required project write scope; there is no default
  CORPUS_NO_TMUX=1     Force the piped run backend"
    ))
}

pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<CliCommand, clap::Error> {
    let matches =
        command().try_get_matches_from(std::iter::once("corpus".to_string()).chain(args))?;
    Cli::from_arg_matches(&matches).map(|cli| cli.command)
}

pub(crate) fn usage() -> String {
    command().render_long_help().to_string()
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn run_parsing_is_typed_and_keeps_multiword_missions() {
        assert_eq!(
            parse(strings(&[
                "run",
                "researcher",
                "inspect",
                "the",
                "parser",
                "--model",
                "ollama/qwen3.8-mlx",
                "--research",
            ]))
            .unwrap(),
            CliCommand::Run(RunArgs {
                agent: "researcher".into(),
                model: Some("ollama/qwen3.8-mlx".into()),
                research: true,
                mission: strings(&["inspect", "the", "parser"]),
            })
        );
    }

    #[test]
    fn run_requires_both_agent_and_mission_and_rejects_unknown_options() {
        for args in [strings(&["run"]), strings(&["run", "researcher"])] {
            assert_eq!(
                parse(args).unwrap_err().kind(),
                ErrorKind::MissingRequiredArgument
            );
        }
        assert_eq!(
            parse(strings(&["run", "researcher", "mission", "--modle", "x"]))
                .unwrap_err()
                .kind(),
            ErrorKind::UnknownArgument
        );
    }

    #[test]
    fn plugin_and_model_subcommands_are_typed() {
        assert_eq!(
            parse(strings(&[
                "plugin",
                "call",
                "fixture",
                "targets",
                "{\"verbose\":true}"
            ]))
            .unwrap(),
            CliCommand::Plugin(PluginArgs {
                command: PluginCommand::Call {
                    name: "fixture".into(),
                    method: "targets".into(),
                    params_json: Some("{\"verbose\":true}".into()),
                }
            })
        );
        assert_eq!(
            parse(strings(&["plugin", "setup", "cdk-regtest"])).unwrap(),
            CliCommand::Plugin(PluginArgs {
                command: PluginCommand::Setup {
                    id: "cdk-regtest".into()
                }
            })
        );
        assert_eq!(
            parse(strings(&["models", "list"])).unwrap(),
            CliCommand::Models(ModelsArgs {
                command: ModelsCommand::List
            })
        );
    }

    #[test]
    fn plugin_arguments_are_required_and_extras_are_rejected() {
        assert_eq!(
            parse(strings(&["plugin", "install"])).unwrap_err().kind(),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse(strings(&["plugin", "install", "cdk-regtest"])).unwrap(),
            CliCommand::Plugin(PluginArgs {
                command: PluginCommand::Install {
                    id: "cdk-regtest".into()
                }
            })
        );
        assert_eq!(
            parse(strings(&[
                "plugin",
                "install-local",
                "/tmp/corpus-plugin-fixture"
            ]))
            .unwrap(),
            CliCommand::Plugin(PluginArgs {
                command: PluginCommand::InstallLocal {
                    bundle_dir: "/tmp/corpus-plugin-fixture".into()
                }
            })
        );
        assert_eq!(
            parse(strings(&["models", "list", "extra"]))
                .unwrap_err()
                .kind(),
            ErrorKind::UnknownArgument
        );
    }

    #[test]
    fn project_commands_type_defaults_options_and_destructive_targets() {
        assert_eq!(
            parse(strings(&["project", "new", "alpha"])).unwrap(),
            CliCommand::Project(ProjectArgs {
                command: ProjectCommand::New {
                    slug: "alpha".into(),
                    name: None,
                    plugin: "cdk-regtest".into(),
                }
            })
        );
        assert_eq!(
            parse(strings(&[
                "project",
                "clone",
                "alpha",
                "--to",
                "beta",
                "--with-corpus",
                "--name",
                "Beta"
            ]))
            .unwrap(),
            CliCommand::Project(ProjectArgs {
                command: ProjectCommand::Clone {
                    slug: "alpha".into(),
                    to: "beta".into(),
                    with_corpus: true,
                    name: Some("Beta".into()),
                }
            })
        );
        assert_eq!(
            parse(strings(&["project", "delete", "alpha"])).unwrap(),
            CliCommand::Project(ProjectArgs {
                command: ProjectCommand::Delete {
                    slug: "alpha".into()
                }
            })
        );
        assert_eq!(
            parse(strings(&["project", "migrate-probes", "alpha", "--apply"])).unwrap(),
            CliCommand::Project(ProjectArgs {
                command: ProjectCommand::MigrateProbes {
                    project: "alpha".into(),
                    apply: true,
                }
            })
        );
    }

    #[test]
    fn project_required_options_fail_in_clap() {
        for args in [
            strings(&["project", "clone", "alpha"]),
            strings(&["project", "rebind", "alpha"]),
        ] {
            assert_eq!(
                parse(args).unwrap_err().kind(),
                ErrorKind::MissingRequiredArgument
            );
        }
    }

    #[test]
    fn agent_commands_type_roles_clone_and_migration_mode() {
        assert_eq!(
            parse(strings(&["agent", "new", "p", "worker"])).unwrap(),
            CliCommand::Agent(AgentArgs {
                command: AgentCommand::New {
                    project: "p".into(),
                    slug: "worker".into(),
                    role: corpus_core::AgentRole::Researcher,
                }
            })
        );
        assert_eq!(
            parse(strings(&["agent", "clone", "p", "worker", "--to", "copy"])).unwrap(),
            CliCommand::Agent(AgentArgs {
                command: AgentCommand::Clone {
                    project: "p".into(),
                    from: "worker".into(),
                    to: "copy".into(),
                }
            })
        );
        assert_eq!(
            parse(strings(&["agent", "migrate-roles", "p", "--apply"])).unwrap(),
            CliCommand::Agent(AgentArgs {
                command: AgentCommand::MigrateRoles {
                    project: "p".into(),
                    apply: true,
                }
            })
        );
    }

    #[test]
    fn agent_roles_and_clone_destination_fail_before_store_access() {
        assert_eq!(
            parse(strings(&["agent", "new", "p", "worker", "--role", "admin"]))
                .unwrap_err()
                .kind(),
            ErrorKind::ValueValidation
        );
        assert_eq!(
            parse(strings(&["agent", "clone", "p", "worker"]))
                .unwrap_err()
                .kind(),
            ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn mission_commands_type_repeatable_pins_and_multiword_briefs() {
        assert_eq!(
            parse(strings(&[
                "mission",
                "new",
                "p",
                "probe",
                "--agent",
                "worker",
                "--budget",
                "20m",
                "--pin",
                "target=main",
                "inspect",
                "the",
                "parser",
                "--pin",
                "tools=v2",
            ]))
            .unwrap(),
            CliCommand::Mission(MissionArgs {
                command: MissionCommand::New {
                    project: "p".into(),
                    slug: "probe".into(),
                    agent: "worker".into(),
                    budget: Some("20m".into()),
                    pins: vec![
                        SourcePin {
                            source: "target".into(),
                            revision: "main".into(),
                        },
                        SourcePin {
                            source: "tools".into(),
                            revision: "v2".into(),
                        },
                    ],
                    brief: strings(&["inspect", "the", "parser"]),
                }
            })
        );
        assert_eq!(
            parse(strings(&["mission", "delete", "p", "probe"])).unwrap(),
            CliCommand::Mission(MissionArgs {
                command: MissionCommand::Delete {
                    project: "p".into(),
                    slug: "probe".into(),
                }
            })
        );
    }

    #[test]
    fn mission_required_values_and_pin_shape_fail_in_clap() {
        for args in [
            strings(&["mission", "new", "p", "probe", "brief"]),
            strings(&["mission", "new", "p", "probe", "--agent", "worker"]),
        ] {
            assert_eq!(
                parse(args).unwrap_err().kind(),
                ErrorKind::MissingRequiredArgument
            );
        }
        assert_eq!(
            parse(strings(&[
                "mission", "new", "p", "probe", "--agent", "worker", "--pin", "main", "brief",
            ]))
            .unwrap_err()
            .kind(),
            ErrorKind::ValueValidation
        );
    }

    #[test]
    fn finding_commands_type_filters_sort_limit_and_paths() {
        assert_eq!(
            parse(strings(&[
                "finding",
                "list",
                "p",
                "--severity",
                "critical,high",
                "--severity",
                "medium",
                "--exclude-unrated",
                "--sort",
                "severity",
                "--limit",
                "5",
                "--text",
                "mint",
            ]))
            .unwrap(),
            CliCommand::Finding(FindingArgs {
                command: FindingCommand::List {
                    project: "p".into(),
                    severities: vec![
                        corpus_core::FindingSeverity::Critical,
                        corpus_core::FindingSeverity::High,
                        corpus_core::FindingSeverity::Medium,
                    ],
                    exclude_unrated: true,
                    text: Some("mint".into()),
                    sort: corpus_core::FindingSort::Severity,
                    limit: Some(5),
                }
            })
        );
        assert_eq!(
            parse(strings(&["finding", "show", "p", "findings/probe.md"])).unwrap(),
            CliCommand::Finding(FindingArgs {
                command: FindingCommand::Show {
                    project: "p".into(),
                    path: "findings/probe.md".into(),
                }
            })
        );
    }

    #[test]
    fn finding_filter_values_and_required_paths_fail_in_clap() {
        for args in [
            strings(&["finding", "list", "p", "--severity", "urgent"]),
            strings(&["finding", "list", "p", "--sort", "risk"]),
            strings(&["finding", "list", "p", "--limit", "0"]),
        ] {
            assert_eq!(parse(args).unwrap_err().kind(), ErrorKind::ValueValidation);
        }
        assert_eq!(
            parse(strings(&["finding", "show", "p"]))
                .unwrap_err()
                .kind(),
            ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn operator_log_arguments_type_defaults_tail_and_refusal_gate() {
        assert_eq!(
            parse(strings(&["audit", "p"])).unwrap(),
            CliCommand::Audit(AuditArgs {
                project: "p".into(),
                tail: 50,
            })
        );
        assert_eq!(
            parse(strings(
                &["refusals", "p", "--tail", "7", "--gate", "role",]
            ))
            .unwrap(),
            CliCommand::Refusals(RefusalsArgs {
                project: "p".into(),
                tail: 7,
                gate: Some(corpus_core::refusal::Gate::Role),
            })
        );
    }

    #[test]
    fn operator_log_required_and_typed_values_fail_in_clap() {
        assert_eq!(
            parse(strings(&["audit"])).unwrap_err().kind(),
            ErrorKind::MissingRequiredArgument
        );
        for args in [
            strings(&["audit", "p", "--tail", "many"]),
            strings(&["refusals", "p", "--gate", "permission"]),
        ] {
            assert_eq!(parse(args).unwrap_err().kind(), ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn generated_help_covers_the_headless_surface_and_environment() {
        let help = usage();
        assert!(help.contains("Usage: corpus <COMMAND>"));
        assert!(help.contains("run"));
        assert!(help.contains("plugin"));
        assert!(help.contains("CORPUS_PROJECT"));
        assert!(!help.contains("corpus [tui]"));
    }
}
