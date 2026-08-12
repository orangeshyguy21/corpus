//! Plugin discovery: a plugins directory holds one subdirectory per
//! plugin, each containing a `plugin.toml` manifest. Probing happens on
//! the host (this module); consumers never spawn plugins themselves.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::plugin::{Plugin, PluginManifest};

/// A discovered plugin directory with a valid manifest.
#[derive(Debug, Clone)]
pub struct PluginDir {
    /// Directory containing the plugin.
    pub dir: PathBuf,
    /// Parsed manifest.
    pub manifest: PluginManifest,
}

/// Environment variable overriding the plugins directory.
pub const PLUGINS_DIR_ENV: &str = "CORPUS_PLUGINS_DIR";

/// Resolve the plugins directory: env override, else `<cwd>/plugins`.
pub fn plugins_dir() -> PathBuf {
    std::env::var(PLUGINS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("plugins"))
}

/// Discover all plugins under a directory (invalid entries are skipped
/// with a warning on stderr, so one bad plugin can't break the registry).
pub fn discover(dir: &Path) -> Result<Vec<PluginDir>, Error> {
    let mut found = Vec::new();
    if !dir.is_dir() {
        return Ok(found);
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    entries.sort();
    for entry in entries {
        match PluginManifest::load(&entry) {
            Ok(manifest) => found.push(PluginDir {
                dir: entry,
                manifest,
            }),
            Err(error) => {
                eprintln!("corpus: skipping {}: {error}", entry.display());
            }
        }
    }
    Ok(found)
}

/// A discovered plugin plus its live probe status — the aggregation the
/// deck renders. spawned/probed on the host inside corpus-core, so the
/// UI never spawns plugins itself (trust domains).
#[derive(Debug, Clone)]
pub struct PluginStatus {
    /// Plugin name (unique within the registry).
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    /// Environment readiness from a live probe.
    pub ready: bool,
    /// Human-readable detail (what is missing, versions, etc.).
    pub notes: String,
}

/// Discover every plugin and probe each one live. Failures are folded
/// into `ready: false` + a note, so one broken plugin can't abort the
/// aggregation.
pub fn plugin_status() -> Vec<PluginStatus> {
    let mut out = Vec::new();
    for plugin in discover(&plugins_dir()).unwrap_or_default() {
        let (ready, notes) = match Plugin::spawn(&plugin.dir) {
            Ok(mut spawned) => match spawned.probe() {
                Ok(result) => (result.ready, result.notes),
                Err(error) => (false, format!("probe failed: {error}")),
            },
            Err(error) => (false, format!("spawn failed: {error}")),
        };
        out.push(PluginStatus {
            name: plugin.manifest.name.clone(),
            version: plugin.manifest.version.clone(),
            description: plugin.manifest.description.clone(),
            ready,
            notes,
        });
    }
    out
}
