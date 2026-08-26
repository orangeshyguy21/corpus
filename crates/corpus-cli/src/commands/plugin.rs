//! Environment-plugin CLI commands.

use std::time::Duration;

use corpus_core::{Plugin, PluginDir, PluginManifestVersion, PluginOrigin};

use crate::cli::PluginCommand;

pub(crate) fn run(command: PluginCommand) -> Result<(), String> {
    // Preserve the existing early catalog validation for every plugin command.
    let plugins = corpus_core::plugin_catalog().map_err(|error| error.to_string())?;
    match command {
        PluginCommand::List => list(&plugins),
        PluginCommand::Install { bundle_dir } => install(&bundle_dir),
        PluginCommand::Select { id, version } => select(&id, &version),
        PluginCommand::Setup { id } => lifecycle(&plugins, "setup", &id),
        PluginCommand::Doctor { id } => lifecycle(&plugins, "doctor", &id),
        PluginCommand::Status { id } => lifecycle(&plugins, "status", &id),
        PluginCommand::Stop { id } => lifecycle(&plugins, "stop", &id),
        PluginCommand::Probe { name } => probe(&plugins, &name),
        PluginCommand::Call {
            name,
            method,
            params_json,
        } => call(&plugins, &name, &method, params_json.as_deref()),
    }
}

fn list(plugins: &[PluginDir]) -> Result<(), String> {
    for plugin in plugins {
        let origin = match plugin.origin {
            PluginOrigin::Direct => "override",
            PluginOrigin::Installed => "installed",
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

fn install(bundle_dir: &std::path::Path) -> Result<(), String> {
    let receipt =
        corpus_core::install_plugin_bundle(bundle_dir).map_err(|error| error.to_string())?;
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

fn select(id: &str, version: &str) -> Result<(), String> {
    corpus_core::select_plugin_version(id, version).map_err(|error| error.to_string())?;
    println!("selected {id}@{version}");
    Ok(())
}

fn lifecycle(plugins: &[PluginDir], method: &str, id: &str) -> Result<(), String> {
    let plugin = find(plugins, id)?;
    let deadline = lifecycle_deadline(method);
    let result = corpus_core::call_plugin_lifecycle_cancellable(
        plugin,
        method,
        deadline,
        || false,
        |progress| eprintln!("[{}] {}", progress.phase, progress.message),
    )
    .map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn probe(plugins: &[PluginDir], name: &str) -> Result<(), String> {
    let plugin_dir = find(plugins, name)?;
    let mut plugin = Plugin::spawn(&plugin_dir.dir).map_err(|error| error.to_string())?;
    if plugin_dir.manifest.manifest_version == PluginManifestVersion::V1 {
        plugin.hello().map_err(|error| error.to_string())?;
        let params =
            corpus_core::plugin_lifecycle_params(plugin_dir).map_err(|error| error.to_string())?;
        let result = plugin
            .lifecycle_call("status", Some(params), Duration::from_secs(10), |_| {})
            .map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
        );
    } else {
        let result = plugin.probe().map_err(|error| error.to_string())?;
        println!("ready: {}\nnotes: {}", result.ready, result.notes);
    }
    Ok(())
}

fn call(
    plugins: &[PluginDir],
    name: &str,
    method: &str,
    params_json: Option<&str>,
) -> Result<(), String> {
    let params = parse_params(params_json)?;
    let plugin_dir = find(plugins, name)?;
    let mut plugin = Plugin::spawn(&plugin_dir.dir).map_err(|error| error.to_string())?;
    let result = if plugin.manifest().manifest_version == PluginManifestVersion::V1 {
        plugin.call_v1(method, params)
    } else {
        plugin.call(method, params)
    }
    .map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn lifecycle_deadline(method: &str) -> Duration {
    if method == "setup" {
        Duration::from_secs(30 * 60)
    } else {
        Duration::from_secs(120)
    }
}

fn parse_params(raw: Option<&str>) -> Result<Option<serde_json::Value>, String> {
    raw.map(|raw| {
        serde_json::from_str(raw).map_err(|error| format!("invalid params json: {error}"))
    })
    .transpose()
}

fn find<'a>(plugins: &'a [PluginDir], name: &str) -> Result<&'a PluginDir, String> {
    let plugin = plugins
        .iter()
        .find(|plugin| plugin.manifest.name == name)
        .ok_or_else(|| format!("plugin not found: {name}"))?;
    corpus_core::verify_plugin_installation(plugin).map_err(|error| error.to_string())?;
    Ok(plugin)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn setup_keeps_the_long_lifecycle_deadline() {
        assert_eq!(lifecycle_deadline("setup"), Duration::from_secs(30 * 60));
        for method in ["doctor", "status", "stop"] {
            assert_eq!(lifecycle_deadline(method), Duration::from_secs(120));
        }
    }

    #[test]
    fn raw_call_params_remain_optional_typed_json() {
        assert_eq!(parse_params(None).unwrap(), None);
        assert_eq!(
            parse_params(Some("{\"verbose\":true}")).unwrap(),
            Some(json!({"verbose": true}))
        );
        let error = parse_params(Some("not-json")).unwrap_err();
        assert!(error.starts_with("invalid params json:"), "{error}");
    }
}
