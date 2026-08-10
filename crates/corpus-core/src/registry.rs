//! Plugin discovery: a plugins directory holds one subdirectory per
//! plugin, each containing a `plugin.toml` manifest.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::plugin::PluginManifest;

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
