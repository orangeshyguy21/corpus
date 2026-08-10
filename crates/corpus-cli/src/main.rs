//! corpus: vulnerability research platform — CLI and TUI entry point.
//!
//! Headless subcommands exist for scripting and debugging; with no
//! subcommand the TUI dashboard launches.

mod tui;

use std::path::PathBuf;
use std::process::ExitCode;

use corpus_core::{discover, plugins_dir, ModelRegistry, Plugin};

const USAGE: &str = "\
corpus — local-first vulnerability research platform

  corpus [tui]                 Launch the TUI dashboard (default)
  corpus plugin list           List discovered environment plugins
  corpus plugin probe <name>   Probe one plugin (environment health)
  corpus plugin call <name> <method> [params-json]
                               Raw protocol call (debugging)
  corpus models list           List the model registry

Environment:
  CORPUS_PLUGINS_DIR           Plugins directory (default: ./plugins)
  CORPUS_MODELS                models.yaml path (default: ./models.yaml)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("tui") => tui::run(),
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
                .unwrap_or_else(|_| PathBuf::from("models.yaml"));
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
