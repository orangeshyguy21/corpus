//! corpus: vulnerability research platform — CLI and TUI entry point.
//!
//! Headless subcommands exist for scripting and debugging; with no
//! subcommand the TUI dashboard launches.

mod tui;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use corpus_core::{discover, plugins_dir, ModelRegistry, Plugin};

const USAGE: &str = "\
corpus — local-first vulnerability research platform

  corpus [tui]                 Launch the TUI dashboard (default)
  corpus run <agent> [-m model] [--research] <mission...>
                               Run an opencode mission; transcript is logged
                               to store/runs/ automatically. --research
                               follows up with a researcher curation pass
                               (technique cards + hypothesis entries).
  corpus plugin list           List discovered environment plugins
  corpus plugin probe <name>   Probe one plugin (environment health)
  corpus plugin call <name> <method> [params-json]
                               Raw protocol call (debugging)
  corpus models list           List the model registry

Environment:
  CORPUS_PLUGINS_DIR           Plugins directory (default: ./plugins)
  CORPUS_MODELS                models.yaml path (default: ./benchmarks/models.yaml)
  CORPUS_STORE                 Store root (default: ~/Sites/corpus/store)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("tui") => tui::run(),
        Some("run") => run_cmd(&args[1..]),
        Some("plugin") => plugin_cmd(&args[1..]),
        Some("models") => models_cmd(&args[1..]),
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
/// opencode mission with the transcript logged to `store/runs/`
/// automatically. Nobody should ever `> /tmp/foo.log` a mission again:
/// runs are corpus data, they belong in the store.
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

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let store = std::env::var("CORPUS_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("{home}/Sites/corpus/store")));
    let runs = store.join("runs");
    std::fs::create_dir_all(&runs).map_err(|e| e.to_string())?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let slug: String = mission
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let log_path = runs.join(format!("{ts}-{agent}-{slug}.log"));

    let header = format!(
        "# corpus run\n# agent: {agent}\n# model: {}\n# started: {ts}\n# mission: {mission}\n\n",
        model.as_deref().unwrap_or("(default)")
    );
    let mut log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    log.write_all(header.as_bytes()).map_err(|e| e.to_string())?;
    println!("logging to {}", log_path.display());

    let status = tee_opencode(&agent, model.as_deref(), &mission, log)?;
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
            log_path.display()
        );
        println!("researching {} ...", log_path.display());
        let status = tee_opencode("researcher", model.as_deref(), &prompt, {
            let mut log = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .map_err(|e| e.to_string())?;
            log.write_all(b"\n\n# --- researcher pass ---\n\n")
                .map_err(|e| e.to_string())?;
            log
        })?;
        if !status.success() {
            return Err(format!("researcher exited with {status}"));
        }
    }
    Ok(())
}

/// Spawn `opencode run`, streaming output to both the terminal and the
/// run log.
fn tee_opencode(
    agent: &str,
    model: Option<&str>,
    prompt: &str,
    log: std::fs::File,
) -> Result<std::process::ExitStatus, String> {
    let mut command = std::process::Command::new("opencode");
    command
        .args(["run", "--agent", agent])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(model) = model {
        command.args(["-m", model]);
    }
    command.arg(prompt);
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn opencode (on PATH?): {e}"))?;

    let log = Arc::new(Mutex::new(log));
    let out = pump_lines(
        child.stdout.take().ok_or("no stdout")?,
        Box::new(std::io::stdout()),
        Arc::clone(&log),
    );
    let err = pump_lines(
        child.stderr.take().ok_or("no stderr")?,
        Box::new(std::io::stderr()),
        Arc::clone(&log),
    );
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = out.join();
    let _ = err.join();
    Ok(status)
}

/// Pump a child output stream to both a terminal writer and the run log.
fn pump_lines<S>(
    stream: S,
    mut term: Box<dyn Write + Send>,
    log: Arc<Mutex<std::fs::File>>,
) -> std::thread::JoinHandle<()>
where
    S: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let _ = writeln!(term, "{line}");
            let _ = term.flush();
            if let Ok(mut log) = log.lock() {
                let _ = writeln!(log, "{line}");
            }
        }
    })
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
