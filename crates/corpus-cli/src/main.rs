//! corpus: vulnerability research platform command-line entry point.
//!
//! The desktop operator UI lives in `corpus-app`; this binary keeps the
//! headless scripting and diagnostic commands.

mod store_admin;

use std::io::Write;
use std::process::ExitCode;

use corpus_core::{ModelRegistry, Plugin, Scope, Store};

/// Built rather than `const` so the role list comes from
/// `AgentRole::names()` — a role the binary supports but the help text
/// omits is a role nobody discovers.
fn usage() -> String {
    let roles = corpus_core::AgentRole::names();
    format!(
        "\
corpus — local-first vulnerability research platform

  corpus                       Show this help
  corpus run <agent> [-m model] [--research] <mission...>
                               Run a mission on the CORPUS_PROJECT
                               scope: the agent materializes to
                               .opencode/agent/ first, the run detaches
                               into a tmux session
                               (corpus-<agent>-<ts>) — attach with
                               `tmux attach -t <name>` any time. No tmux?
                               degrades to a piped spawn (no attach).
                               Transcript: project corpus runs/.
  corpus plugin list           List discovered environment plugins
  corpus plugin install <dir>  Atomically install and select a local v1 bundle
  corpus plugin select <id> <version>
                               Select an installed version (upgrade/rollback)
  corpus plugin setup|doctor|status|stop <id>
                               Manage plugin readiness and shared resources
  corpus plugin probe <name>   Probe one plugin (environment health)
  corpus plugin call <name> <method> [params-json]
                               Raw protocol call (debugging)
  corpus models list           List the model registry
  corpus project list|new|clone|delete|wipe|rebind
                               Project CRUD (store/projects/<slug>/)
  corpus agent list|new|clone|delete <project> ...
                               Agent CRUD (store/projects/<p>/agents/<slug>/).
                               `new` takes --role {roles}
                               (default researcher): the role supplies the
                               starting prompt AND the capability ceiling.
  corpus agent role <project> <slug> [{roles}]
                               Show or set an agent's ROLE — the capability
                               ceiling corpus-mcp enforces server-side.
  corpus agent migrate-roles <project> [--apply]
                               Assign roles to agents predating the role
                               system, inferred from what their permissions
                               already grant. Dry run without --apply.
  corpus mission list|new|delete <project> ...
                               Mission CRUD
  corpus finding list <project> [--severity <level>] [--exclude-unrated]
                               [--text <query>] [--sort newest|severity] [--limit N]
  corpus finding show <project> <findings/path.md>
                               Discover or read findings through the shared
                               tolerant metadata projection.
  corpus audit <project> [--tail N]
                               Who changed this project, and when. Every
                               mutation a `curator` agent makes is recorded
                               here (intent, then outcome) — no agent can
                               read or edit this log.
  corpus refusals <project> [--tail N] [--gate G]
                               What the server turned away, and which gate
                               did it: identity, role, scope, probe, args,
                               unknown, harness. Read this before the
                               transcript — no refusals here means the run
                               was stopped somewhere other than corpus.
                               Calls refused before a project could be
                               resolved are under `_unscoped`.

Environment:
  CORPUS_HOME                  Data root (default: ~/.corpus) — projects,
                               run dirs, chat scopes, app prefs
  CORPUS_STORE                 Store root (default: <CORPUS_HOME>/store).
                               Moves run dirs and chat scopes with it.
  CORPUS_RESOURCES             Shipped resource root (default: found from the executable)
  CORPUS_PLUGINS_DIR           Complete development/test catalog override
  CORPUS_SOURCES_DIR           Pinned-source cache override
  CORPUS_MODELS                models.yaml override (default:
                               <CORPUS_RESOURCES>/benchmarks/models.yaml)
  CORPUS_PROJECT               Write scope. NO DEFAULT — every command that
                               writes refuses without it.
  CORPUS_TERMINAL              Terminal app for `attach` (default: from $TERM_PROGRAM)
  CORPUS_NO_TMUX=1             Force the piped run backend (no detached sessions)"
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => {
            println!("{}", usage());
            Ok(())
        }
        Some("run") => run_cmd(&args[1..]),
        Some("plugin") => plugin_cmd(&args[1..]),
        Some("models") => models_cmd(&args[1..]),
        Some("project") => store_admin::project_cmd(&args[1..]),
        Some("agent") => store_admin::agent_cmd(&args[1..]),
        Some("mission") => store_admin::mission_cmd(&args[1..]),
        Some("finding") => store_admin::finding_cmd(&args[1..]),
        Some("audit") => store_admin::audit_cmd(&args[1..]),
        Some("refusals") => store_admin::refusals_cmd(&args[1..]),
        Some("help") | Some("--help") | Some("-h") => {
            println!("{}", usage());
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}\n\n{}", usage())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("corpus: error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod usage_tests {
    use super::usage;

    #[test]
    fn bare_cli_help_documents_headless_surface_only() {
        let help = usage();
        assert!(help.contains("corpus run <agent>"));
        assert!(help.contains("corpus plugin probe <name>"));
        assert!(!help.contains("corpus [tui]"));
    }
}

/// `corpus run <agent> [-m model] [--research] <mission...>` — run an
/// opencode mission with the transcript logged to the project corpus
/// `runs/` automatically. The agent is materialized to `.opencode/agent/`
/// first (bare names), then opencode spawns with CORPUS_PROJECT set,
/// so the MCP server writes into the project scope.
fn run_cmd(args: &[String]) -> Result<(), String> {
    let mut agent: Option<String> = None;
    let mut model: Option<String> = None;
    let mut research = false;
    let mut mission_words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-m" | "--model" => {
                model = Some(args.get(i + 1).ok_or("missing model after -m")?.clone());
                i += 2;
            }
            "--research" => {
                research = true;
                i += 1;
            }
            word if agent.is_none() => {
                agent = Some(word.to_string());
                i += 1;
            }
            word => {
                mission_words.push(word.to_string());
                i += 1;
            }
        }
    }
    let agent = agent.ok_or("usage: corpus run <agent> [-m model] [--research] <mission...>")?;
    let mission = mission_words.join(" ");
    if mission.trim().is_empty() {
        return Err("usage: corpus run <agent> [-m model] [--research] <mission...>".to_string());
    }

    let store = Store::from_env();
    // No default project: a run that cannot name its project would write a
    // whole mission's output into someone else's corpus.
    let scope = Scope::from_env_strict(&store)?;

    // Check the agent exists on the project.
    if store.load_agent(&scope.project, &agent).is_err() {
        let agents = store
            .list_agents(&scope.project)
            .map_err(|e| e.to_string())?;
        let have: Vec<String> = agents.iter().map(|(s, _)| s.clone()).collect();
        return Err(format!(
            "project {} has no agent named {:?} (agents: {})",
            scope.project,
            agent,
            have.join(", ")
        ));
    }

    // Materialize the project's agents (clear + render to .opencode/agent/):
    // the agent list opencode shows is scoped to the project.
    let written = store
        .render_project_agents(&scope.project)
        .map_err(|e| e.to_string())?;
    for path in &written {
        println!("materialized {}", path.display());
    }

    let project_record =
        corpus_core::Project::load(&store, &scope.project).map_err(|error| error.to_string())?;
    let resolved = corpus_core::prepare_source_pins(&store, &scope.project, &project_record.pins)
        .map_err(|error| error.to_string())?;
    let pins_json = (!resolved.is_empty())
        .then(|| serde_json::to_string(&resolved))
        .transpose()
        .map_err(|error| error.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let environment_id = corpus_core::EnvironmentSessionId {
        project: scope.project.clone(),
        mission: format!("headless-{stamp}"),
        generation: stamp,
    };
    let mut environment = corpus_core::open_environment_session(&store, environment_id, resolved)
        .map_err(|error| error.to_string())?;
    let environment_key = environment.as_ref().map(|record| record.id.storage_key());
    let run_result = (|| {
        let mut session = corpus_core::RunSession::spawn_headless_with_environment(
            &scope.project,
            &agent,
            model.as_deref(),
            &mission,
            pins_json.as_deref(),
            environment_key.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        println!("logging to {}", session.transcript.display());
        let status = drain_session(&mut session)?;
        if !status.success() {
            return Err(format!("opencode exited with {status}"));
        }

        if research {
            let prompt = format!(
                "Read the operator run transcript at {}. Curate and distill it \
             into the corpus: (1) technique card(s) under the project corpus \
             techniques/ for every attack surface analyzed or attempted — \
             mechanics, preconditions, the oracle that would catch it, and \
             status (fired / analyzed-only / unresolved-lead) — citing this \
             run log. (2) A 'failure modes' section per card citing transcript \
             moments. (3) An honest run outcome at the end of each card: \
             completed / truncated / blocked, and why. (4) If the transcript \
             yields a fresh lead worth attacking, write one hypothesis entry \
             under the project corpus hypotheses/ (target surface, rationale, \
             suggested mission text, source citations). Remember the \
             contamination rule: never read benchmarks/** or plugins/**.",
                session.transcript.display()
            );
            println!("researching {} ...", session.transcript.display());
            let mut log = std::fs::OpenOptions::new()
                .append(true)
                .open(&session.transcript)
                .map_err(|e| e.to_string())?;
            log.write_all(b"\n\n# --- researcher pass ---\n\n")
                .map_err(|e| e.to_string())?;
            drop(log);
            // Materialize the researcher agent for the follow-up.
            let _ = store
                .render_agent(&scope.project, "researcher")
                .map_err(|e| e.to_string())?;
            let mut session = corpus_core::RunSession::spawn_headless_append_with_environment(
                &scope.project,
                "researcher",
                model.as_deref(),
                &prompt,
                &session.transcript,
                pins_json.as_deref(),
                environment_key.as_deref(),
            )
            .map_err(|e| e.to_string())?;
            let status = drain_session(&mut session)?;
            if !status.success() {
                return Err(format!("researcher exited with {status}"));
            }
        }
        Ok(())
    })();
    let close_result = environment.as_mut().map(|environment| {
        corpus_core::close_environment_session(&store, environment)
            .map_err(|error| error.to_string())
    });
    match (run_result, close_result) {
        (Ok(()), None | Some(Ok(()))) => Ok(()),
        (Ok(()), Some(Err(cleanup))) => Err(cleanup),
        (Err(run), None | Some(Ok(()))) => Err(run),
        (Err(run), Some(Err(cleanup))) => {
            Err(format!("{run}; environment cleanup also failed: {cleanup}"))
        }
    }
}

/// Pump a session's lines to the terminal until it exits, then flush
/// whatever the pumps still held.
fn drain_session(
    session: &mut corpus_core::RunSession,
) -> Result<std::process::ExitStatus, String> {
    loop {
        while let Some(line) = session.poll_line() {
            if line.stderr {
                eprintln!("{}", line.text);
            } else {
                println!("{}", line.text);
            }
        }
        if let Some(status) = session.try_exit() {
            while let Some(line) = session.poll_line_timeout(std::time::Duration::from_millis(400))
            {
                if line.stderr {
                    eprintln!("{}", line.text);
                } else {
                    println!("{}", line.text);
                }
            }
            return Ok(status);
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}

/// `corpus plugin ...` subcommands.
fn plugin_cmd(args: &[String]) -> Result<(), String> {
    let plugins = corpus_core::plugin_catalog().map_err(|e| e.to_string())?;
    match args.first().map(String::as_str) {
        Some("list") => {
            for plugin in &plugins {
                let origin = match plugin.origin {
                    corpus_core::PluginOrigin::Direct => "override",
                    corpus_core::PluginOrigin::Installed => "installed",
                    corpus_core::PluginOrigin::Bundled => "bundled",
                };
                println!(
                    "{:<20} {:<8} {:<9} {}\n  {}",
                    plugin.manifest.name,
                    plugin.manifest.version.as_deref().unwrap_or("-"),
                    origin,
                    plugin.dir.display(),
                    plugin.manifest.description.as_deref().unwrap_or("")
                );
            }
            if plugins.is_empty() {
                println!(
                    "no plugins found (install root: {})",
                    corpus_core::plugin_install_root().display()
                );
            }
            Ok(())
        }
        Some("install") => {
            let bundle = args
                .get(1)
                .ok_or("usage: corpus plugin install <bundle-dir>")?;
            let receipt = corpus_core::install_plugin_bundle(std::path::Path::new(bundle))
                .map_err(|error| error.to_string())?;
            println!(
                "installed {}@{}\ndigest: {}\npath: {}\nprevious: {}",
                receipt.id,
                receipt.version,
                receipt.digest,
                receipt.path.display(),
                receipt.previous.as_deref().unwrap_or("none")
            );
            Ok(())
        }
        Some("select") => {
            let id = args
                .get(1)
                .ok_or("usage: corpus plugin select <id> <version>")?;
            let version = args
                .get(2)
                .ok_or("usage: corpus plugin select <id> <version>")?;
            corpus_core::select_plugin_version(id, version).map_err(|error| error.to_string())?;
            println!("selected {id}@{version}");
            Ok(())
        }
        Some(method @ ("setup" | "doctor" | "status" | "stop")) => {
            let name = args
                .get(1)
                .ok_or_else(|| format!("usage: corpus plugin {method} <id>"))?;
            let plugin_dir = find(&plugins, name)?;
            let deadline = if method == "setup" {
                std::time::Duration::from_secs(30 * 60)
            } else {
                std::time::Duration::from_secs(120)
            };
            let result = corpus_core::call_plugin_lifecycle_cancellable(
                plugin_dir,
                method,
                deadline,
                || false,
                |progress| {
                    eprintln!("[{}] {}", progress.phase, progress.message);
                },
            )
            .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        Some("probe") => {
            let name = args.get(1).ok_or("usage: corpus plugin probe <name>")?;
            let plugin_dir = find(&plugins, name)?;
            let mut plugin = Plugin::spawn(&plugin_dir.dir).map_err(|e| e.to_string())?;
            if plugin_dir.manifest.manifest_version == corpus_core::PluginManifestVersion::V1 {
                plugin.hello().map_err(|error| error.to_string())?;
                let params = corpus_core::plugin_lifecycle_params(plugin_dir)
                    .map_err(|error| error.to_string())?;
                let result = plugin
                    .lifecycle_call(
                        "status",
                        Some(params),
                        std::time::Duration::from_secs(10),
                        |_| {},
                    )
                    .map_err(|error| error.to_string())?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
                );
            } else {
                let result = plugin.probe().map_err(|e| e.to_string())?;
                println!("ready: {}\nnotes: {}", result.ready, result.notes);
            }
            Ok(())
        }
        Some("call") => {
            let name = args
                .get(1)
                .ok_or("usage: corpus plugin call <name> <method> [params-json]")?;
            let method = args
                .get(2)
                .ok_or("usage: corpus plugin call <name> <method> [params-json]")?;
            let params = match args.get(3) {
                Some(raw) => Some(
                    serde_json::from_str(raw).map_err(|e| format!("invalid params json: {e}"))?,
                ),
                None => None,
            };
            let plugin_dir = find(&plugins, name)?;
            let mut plugin = Plugin::spawn(&plugin_dir.dir).map_err(|e| e.to_string())?;
            let result =
                if plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1 {
                    plugin.call_v1(method, params)
                } else {
                    plugin.call(method, params)
                }
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        _ => Err(
            "usage: corpus plugin list|install|select|setup|doctor|status|stop|probe|call ..."
                .to_string(),
        ),
    }
}

/// `corpus models list`.
fn models_cmd(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => {
            let registry = ModelRegistry::load_default().map_err(|e| e.to_string())?;
            for model in &registry.models {
                println!(
                    "{:<20} {:<8} {:<10} {}",
                    model.tag,
                    model
                        .params_b
                        .map(|p| format!("{p}B"))
                        .unwrap_or_else(|| "-".to_string()),
                    model.provider,
                    model.capabilities.join(",")
                );
            }
            if registry.models.is_empty() {
                println!("no models registered");
            }
            Ok(())
        }
        _ => Err("usage: corpus models list".to_string()),
    }
}

/// Find a discovered plugin by name.
fn find<'a>(
    plugins: &'a [corpus_core::PluginDir],
    name: &str,
) -> Result<&'a corpus_core::PluginDir, String> {
    plugins
        .iter()
        .find(|p| p.manifest.name == name)
        .ok_or_else(|| format!("plugin not found: {name}"))
}
