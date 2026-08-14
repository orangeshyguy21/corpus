//! Plugin discovery: a plugins directory holds one subdirectory per
//! plugin, each containing a `plugin.toml` manifest. Probing happens on
//! the host (this module); consumers never spawn plugins themselves.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Error;
use crate::plugin::{Plugin, PluginManifest};
use crate::store::Store;

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
/// app renders. spawned/probed on the host inside corpus-core, so the
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

/// One source repo a project's plugin mounts, with its selectable revs —
/// the top bar's per-source dropdowns (data-model v2 decision 6: missions
/// carry per-source repo→rev pins, defaulting to the plugin's pin).
#[derive(Debug, Clone)]
pub struct SourceRevs {
    /// Repository name (`cdk`, `nuts`).
    pub name: String,
    /// The plugin's pinned revision from the repo's `sources.toml` — the
    /// default the top bar offers (falls back to `main` when unknown).
    pub pinned: String,
    /// Selectable revisions: `sources.toml` tags plus the `main` branch.
    pub revs: Vec<String>,
}

/// The source repos a project's plugin declares in its `config.toml`
/// `[sources]` table (`<repo>_sha` keys), each with the revisions the top
/// bar may offer — the repo's `sources.toml` tag plus `main`. Returns an
/// empty vec (never an error) when the project's plugin or its
/// `config.toml`/`[sources]` can't be found — the top bar then keeps its
/// placeholder pins rather than failing. A missing project is an error.
pub fn plugin_sources(store: &Store, project: &str) -> Result<Vec<SourceRevs>, Error> {
    let spec = crate::store::Project::load(store, project)?;
    let Some(pdir) = discover(&plugins_dir())?
        .into_iter()
        .find(|p| p.manifest.name == spec.plugin)
    else {
        return Ok(Vec::new());
    };
    let raw = fs::read_to_string(pdir.dir.join("config.toml"))
        .map_err(|e| Error::Store(format!("plugin {} config.toml: {e}", spec.plugin)))?;
    let config: toml::Value = toml::from_str(&raw)
        .map_err(|e| Error::Store(format!("plugin {} config.toml: {e}", spec.plugin)))?;
    let sources = config
        .get("sources")
        .and_then(|s| s.as_table())
        .ok_or_else(|| Error::Store(format!("plugin {} has no [sources] table", spec.plugin)))?;
    let mut repos: Vec<String> = sources
        .keys()
        .filter(|k| k.ends_with("_sha"))
        .map(|k| k.trim_end_matches("_sha").to_string())
        .collect();
    repos.sort();

    // The repo's sources.toml (sibling of the plugins dir, at the repo
    // root) provides the tags to offer alongside `main`.
    let repo_root = pdir
        .dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| pdir.dir.clone());
    let tags = load_source_tags(&repo_root.join("sources.toml"));

    let mut out = Vec::new();
    for repo in repos {
        let pinned = tags.get(&repo).cloned().unwrap_or_else(|| "main".to_string());
        let mut revs = vec![pinned.clone()];
        if pinned != "main" {
            revs.push("main".to_string());
        }
        out.push(SourceRevs { name: repo, pinned, revs });
    }
    Ok(out)
}

/// The root `sources.toml`: `[sources.<name>]` → repo / tag / sha.
#[derive(Deserialize)]
struct SourceFile {
    #[serde(default)]
    sources: BTreeMap<String, SourceEntry>,
}

#[derive(Deserialize)]
#[allow(dead_code)] // repo/sha are parsed for `tag` lookup but not read here
struct SourceEntry {
    #[serde(default)]
    repo: String,
    tag: String,
    #[serde(default)]
    sha: String,
}

/// Best-effort map of repo name → tag from a repo-root `sources.toml`.
/// Missing/unparseable file yields an empty map (revs degrade to `main`).
fn load_source_tags(path: &Path) -> BTreeMap<String, String> {
    let raw = match fs::read_to_string(path) {
        Ok(r) => r,
        Err(_) => return BTreeMap::new(),
    };
    let parsed: SourceFile = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(_) => return BTreeMap::new(),
    };
    parsed.sources.into_iter().map(|(name, entry)| (name, entry.tag)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn plugin_sources_reads_config_and_tags() {
        let root = std::env::temp_dir().join(format!("corpus-plugin-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // A fake plugin tree: <root>/plugins/cdk-regtest/ ; repo root = <root>.
        let pdir = root.join("plugins").join("cdk-regtest");
        write(&pdir.join("plugin.toml"), "name = \"cdk-regtest\"\nexec = \"plugin\"\n");
        write(
            &pdir.join("config.toml"),
            "[sources]\ncdk_sha = \"86a7c6\"\nnuts_sha = \"3bc8b6\"\n",
        );
        write(
            &root.join("sources.toml"),
            "[sources.cdk]\nrepo = \"cashubtc/cdk\"\ntag = \"v0.17.0\"\nsha = \"86a7c6cacb\"\n\
             [sources.nuts]\nrepo = \"cashubtc/nuts\"\ntag = \"main\"\nsha = \"3bc8b6d5\"\n",
        );
        std::env::set_var("CORPUS_PLUGINS_DIR", root.join("plugins"));

        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let revs = plugin_sources(&store, "p").unwrap();
        assert_eq!(revs.len(), 2, "config.toml declares cdk + nuts");
        let cdk = revs.iter().find(|s| s.name == "cdk").unwrap();
        assert_eq!(cdk.pinned, "v0.17.0");
        assert_eq!(cdk.revs, vec!["v0.17.0".to_string(), "main".to_string()]);
        let nuts = revs.iter().find(|s| s.name == "nuts").unwrap();
        assert_eq!(nuts.pinned, "main");
        assert_eq!(nuts.revs, vec!["main".to_string()]);

        // A project bound to a missing plugin degrades to empty, not error.
        store.create_project("q", "Q", "ghost-plugin").unwrap();
        assert!(plugin_sources(&store, "q").unwrap().is_empty());
        // A missing project errors.
        assert!(plugin_sources(&store, "ghost").is_err());

        std::env::remove_var("CORPUS_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }
}
