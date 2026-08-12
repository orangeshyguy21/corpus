//! corpus: vulnerability research platform — CLI and TUI entry point.
//!
//! Headless subcommands exist for scripting and debugging; with no
//! subcommand the TUI dashboard launches.

mod store_admin;
mod tui;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use corpus_core::{discover, plugins_dir, ModelRegistry, Plugin, Scope, Store};

const USAGE: &str = "\
corpus — local-first vulnerability research platform

  corpus [tui]                 Launch the TUI dashboard (default)
  corpus run <agent> [-m model] [--research] <mission...>
                               Run a mission on the CORPUS_PROJECT/TEAM
                               scope: agents materialize to
                               .opencode/agent/ first, the run detaches
                               into a tmux session
                               (corpus-<team>-<agent>-<ts>) — attach with
                               `tmux attach -t <name>` any time. No tmux?
                               degrades to a piped spawn (no attach).
                               Transcript: team corpus runs/.
  corpus plugin list           List discovered environment plugins
  corpus plugin probe <name>   Probe one plugin (environment health)
  corpus plugin call <name> <method> [params-json]
                               Raw protocol call (debugging)
  corpus models list           List the model registry
  corpus project list|new|clone|delete
                               Project CRUD (store/projects/<slug>/)
  corpus team list|new|edit|clone|delete|wipe <project> ...
                               Team CRUD + corpus wipe (generation counter)
  corpus template list|render  Core/project templates + render to .opencode/agent/
  corpus promote <project> <team> <category> <entry> [--confirm]
                               Lift a team entry into the project corpus
  corpus store migrate         Relocate a legacy flat store into the default
                               project (projects/<slug>/corpus/)

Environment:
  CORPUS_PLUGINS_DIR           Plugins directory (default: ./plugins)
  CORPUS_MODELS                models.yaml path (default: ./benchmarks/models.yaml)
  CORPUS_STORE                 Store root (default: ~/Sites/corpus/store)
  CORPUS_PROJECT, CORPUS_TEAM  Default write/promote scope (default: default/default)
  CORPUS_TERMINAL              Terminal app for `attach` (default: from $TERM_PROGRAM)
  CORPUS_NO_TMUX=1             Force the piped run backend (no detached sessions)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("tui") => tui::run(),
        Some("run") => run_cmd(&args[1..]),
        Some("plugin") => plugin_cmd(&args[1..]),
        Some("models") => models_cmd(&args[1..]),
        Some("project") => store_admin::project_cmd(&args[1..]),
        Some("team") => store_admin::team_cmd(&args[1..]),
        Some("template") => store_admin::template_cmd(&args[1..]),
        Some("promote") => store_admin::promote_cmd(&args[1..]),
        Some("store") => store_admin::store_cmd(&args[1..]),
        Some("help") | Some("--help") | Some("-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("corpus: error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// `corpus run <agent> [-m model] [--research] <mission...>` — run an
/// opencode mission with the transcript logged to the scoped store
/// `runs/` automatically. The team's agents are materialized to
/// `.opencode/agent/` first (default team: bare names; other teams:
/// `<team>-<agent>.md`), then the same session handle the deck uses
/// spawns opencode with CORPUS_PROJECT/CORPUS_TEAM set, so the MCP
/// server writes into the team scope.
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
    let scope = Scope::from_env();
    let spec = corpus_core::TeamSpec::load(&store, &scope.project, &scope.team)
        .map_err(|e| e.to_string())?;
    if !spec.agents.contains_key(&agent) {
        let have: Vec<String> = spec.agents.keys().cloned().collect();
        return Err(format!(
            "team {}/{} has no agent named {:?} (agents: {})",
            scope.project, scope.team, agent, have.join(", ")
        ));
    }
    // Materialize the team's agents first (the default team re-renders
    // the checked-in pair byte-identically; other teams get
    // `<team>-<agent>.md`).
    let written = store
        .materialize_team_agents(&scope.project, &scope.team)
        .map_err(|e| e.to_string())?;
    for path in &written {
        println!("materialized {}", path.display());
    }

    let mut session = corpus_core::RunSession::spawn_headless(
        &scope.project,
        &scope.team,
        &agent,
        model.as_deref(),
        &mission,
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
             into the corpus: (1) technique card(s) under store/techniques/ \
             for every attack surface analyzed or attempted — mechanics, \
             preconditions, the oracle that would catch it, and status \
             (fired / analyzed-only / unresolved-lead) — citing this run \
             log. (2) A 'failure modes' section per card citing transcript \
             moments. (3) An honest run outcome at the end of each card: \
             completed / truncated / blocked, and why. (4) If the \
             transcript yields a fresh lead worth attacking, write one \
             hypothesis entry under store/hypotheses/ (target surface, \
             rationale, suggested mission text, source citations). \
             Remember the contamination rule: never read benchmarks/** or \
             plugins/**.",
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
        let mut session = corpus_core::RunSession::spawn_headless_append(
            &scope.project,
            &scope.team,
            "researcher",
            model.as_deref(),
            &prompt,
            &session.transcript,
        )
        .map_err(|e| e.to_string())?;
        let status = drain_session(&mut session)?;
        if !status.success() {
            return Err(format!("researcher exited with {status}"));
        }
    }
    Ok(())
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
    let dir = plugins_dir();
    let plugins = discover(&dir).map_err(|e| e.to_string())?;
    match args.first().map(String::as_str) {
        Some("list") => {
            for plugin in &plugins {
                println!(
                    "{:<20} {:<8} {}",
                    plugin.manifest.name,
                    plugin.manifest.version.as_deref().unwrap_or("-"),
                    plugin.manifest.description.as_deref().unwrap_or("")
                );
            }
            if plugins.is_empty() {
                println!("no plugins found in {}", dir.display());
            }
            Ok(())
        }
        Some("probe") => {
            let name = args.get(1).ok_or("usage: corpus plugin probe <name>")?;
            let plugin_dir = find(&plugins, name)?;
            let mut plugin = Plugin::spawn(&plugin_dir.dir).map_err(|e| e.to_string())?;
            let result = plugin.probe().map_err(|e| e.to_string())?;
            println!("ready: {}\nnotes: {}", result.ready, result.notes);
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
            let result = plugin.call(method, params).map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        _ => Err("usage: corpus plugin list|probe|call ...".to_string()),
    }
}

/// `corpus models list`.
fn models_cmd(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => {
            let path = std::env::var("CORPUS_MODELS")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("benchmarks/models.yaml"));
            let registry = ModelRegistry::load(&path).map_err(|e| e.to_string())?;
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
                println!("no models in {}", path.display());
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
