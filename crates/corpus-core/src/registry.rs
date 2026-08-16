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
    /// Selectable revisions, DEFAULT FIRST: `main`/`master` leads when
    /// its sha is known (live ls-remote or cache), then the manifest
    /// pin, then the remaining remote tags newest-first; offline with no
    /// cache degrades to pin + main with the pin leading (a default must
    /// resolve without the network).
    pub revs: Vec<String>,
    /// Epoch seconds when the rev list was last fetched live
    /// (`sources/.rev-cache/<name>.json`); None = no cache, the revs are
    /// pin+main placeholders. The top bar shows a stale-cache hint next
    /// to a branch rev selected from an aged list.
    pub refs_fetched: Option<u64>,
}

impl SourceRevs {
    /// The rev a fresh picker selects: the first list entry. `main` when
    /// resolvable, else the manifest pin.
    pub fn default_rev(&self) -> &str {
        self.revs.first().map(String::as_str).unwrap_or(&self.pinned)
    }
}

/// The source repos a project's plugin declares in its `config.toml`
/// `[sources]` table (`<repo>_sha` keys), each with the revisions the top
/// bar may offer: the manifest pin plus the live remote tag set (cached
/// under `sources/.rev-cache/`). Returns an empty vec (never an error)
/// when the project's plugin or its `config.toml`/`[sources]` can't be
/// found — the top bar then keeps its placeholder pins rather than
/// failing. A missing project is an error.
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
    // root) provides each source's pin identity + the clone URL; fetched
    // trees (and the rev cache) live beside it under sources/.
    let repo_root = pdir
        .dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| pdir.dir.clone());
    let sources_dir = repo_root.join("sources");
    let entries = load_source_entries(&repo_root.join("sources.toml"));

    let mut out = Vec::new();
    for repo in repos {
        let entry = entries.get(&repo);
        let pinned = entry
            .map(|e| e.tag.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "main".to_string());
        let revs = match entry {
            Some(entry) if !entry.repo.is_empty() => {
                crate::srcrev::selectable_revs(&sources_dir, &repo, &entry.repo, &pinned)
            }
            _ => {
                let mut revs = vec![pinned.clone()];
                if pinned != "main" {
                    revs.push("main".to_string());
                }
                revs
            }
        };
        let fetched = crate::srcrev::revs_cache_fetched(&sources_dir, &repo);
        out.push(SourceRevs {
            name: repo,
            pinned,
            revs,
            refs_fetched: fetched,
        });
    }
    Ok(out)
}

/// Resolve a mission's `repo → rev` pins to `repo → sha`, fetching any
/// source tree not yet materialized under `sources/`. Launch calls this
/// so the sha set is fixed at pick time (a branch pin records where the
/// branch WAS) — failure is loud: a run never silently mounts a rev the
/// mission didn't record. Unknown repos (not declared by the plugin's
/// `[sources]`) are ignored, not errors — a stale pin for a removed
/// source must not brick launches.
pub fn prepare_source_pins(
    store: &Store,
    project: &str,
    pins: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, Error> {
    let mut resolved = BTreeMap::new();
    if pins.is_empty() {
        return Ok(resolved);
    }
    let spec = crate::store::Project::load(store, project)?;
    let Some(pdir) = discover(&plugins_dir())?
        .into_iter()
        .find(|p| p.manifest.name == spec.plugin)
    else {
        return Err(Error::Store(format!(
            "mission carries source pins but plugin {} is not discovered",
            spec.plugin
        )));
    };
    let raw = fs::read_to_string(pdir.dir.join("config.toml"))
        .map_err(|e| Error::Store(format!("plugin {} config.toml: {e}", spec.plugin)))?;
    let config: toml::Value = toml::from_str(&raw)
        .map_err(|e| Error::Store(format!("plugin {} config.toml: {e}", spec.plugin)))?;
    let declared: Vec<String> = config
        .get("sources")
        .and_then(|s| s.as_table())
        .map(|t| {
            t.keys()
                .filter(|k| k.ends_with("_sha"))
                .map(|k| k.trim_end_matches("_sha").to_string())
                .collect()
        })
        .unwrap_or_default();
    let repo_root = pdir
        .dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| pdir.dir.clone());
    let sources_dir = repo_root.join("sources");
    let entries = load_source_entries(&repo_root.join("sources.toml"));
    for (name, rev) in pins {
        if rev.trim().is_empty() || !declared.contains(name) {
            continue;
        }
        let Some(entry) = entries.get(name).filter(|e| !e.repo.is_empty()) else {
            return Err(Error::Store(format!(
                "source pin {name}: no repo URL in sources.toml"
            )));
        };
        // The manifest tag is the audited default: normally resolve it
        // sha-direct, no ls-remote needed — the default path must work
        // offline. EXCEPTION: a manifest "tag" that is itself a branch
        // (`main`/`master`) is MUTABLE — the recorded sha is a
        // setup-time freeze, not the tip (cdk's sources.toml pins
        // `main` for the spec), so branch-valued defaults resolve
        // through the rev cache like any other branch pin. Offline with
        // no cache at all, the recorded default sha is the graceful
        // fallback — but ONLY for the manifest's own rev; any other pin
        // stays a loud error.
        let branch_default = matches!(entry.tag.as_str(), "main" | "master");
        let sha = if rev == &entry.tag && !entry.sha.is_empty() && !branch_default {
            entry.sha.clone()
        } else {
            match crate::srcrev::resolve_rev(&sources_dir, name, &entry.repo, rev) {
                Ok(sha) => sha,
                Err(err) => {
                    if rev == &entry.tag && !entry.sha.is_empty() {
                        entry.sha.clone()
                    } else {
                        return Err(err);
                    }
                }
            }
        };
        crate::srcrev::ensure_source_tree(&sources_dir, name, &entry.repo, rev, &sha)?;
        resolved.insert(name.clone(), sha);
    }
    Ok(resolved)
}

/// The root `sources.toml`: `[sources.<name>]` → repo / tag / sha.
#[derive(Deserialize)]
struct SourceFile {
    #[serde(default)]
    sources: BTreeMap<String, SourceEntry>,
}

#[derive(Deserialize)]
struct SourceEntry {
    #[serde(default)]
    repo: String,
    tag: String,
    /// The manifest pin's sha — the offline-runnable default resolution.
    #[serde(default)]
    sha: String,
}

/// Best-effort map of repo name → manifest entry from a repo-root
/// `sources.toml`. Missing/unparseable file yields an empty map (revs
/// degrade to the pin + `main`).
fn load_source_entries(path: &Path) -> BTreeMap<String, SourceEntry> {
    let raw = match fs::read_to_string(path) {
        Ok(r) => r,
        Err(_) => return BTreeMap::new(),
    };
    let parsed: SourceFile = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(_) => return BTreeMap::new(),
    };
    parsed.sources
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registry tests mutate the process-global `CORPUS_PLUGINS_DIR`, so
    /// they must not race the parallel test pool (same pattern as the
    /// env-mutating launch tests).
    static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A local bare-ish fixture remote (path URL) with tags, so rev
    /// discovery never touches the network in tests. Returns the repo
    /// path plus the sha `extra_tag` points at.
    fn fixture_remote(root: &Path, extra_tag: &str) -> (String, String) {
        let work = root.join("fixture-work");
        std::fs::create_dir_all(&work).unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap();
            assert!(status.success());
        };
        run(&["init", "--quiet", "-b", "main"]);
        run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--quiet", "--allow-empty", "-m", "one"]);
        run(&["tag", "v0.1.0"]);
        run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--quiet", "--allow-empty", "-m", "two"]);
        run(&["tag", extra_tag]);
        let sha = std::process::Command::new("git")
            .args(["rev-parse", extra_tag])
            .current_dir(&work)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap();
        (work.to_string_lossy().into_owned(), sha)
    }

    #[test]
    fn plugin_sources_reads_config_and_tags() {
        let _guard = env_lock();
        let root = std::env::temp_dir().join(format!("corpus-plugin-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // A fake plugin tree: <root>/plugins/cdk-regtest/ ; repo root = <root>.
        let pdir = root.join("plugins").join("cdk-regtest");
        write(&pdir.join("plugin.toml"), "name = \"cdk-regtest\"\nexec = \"plugin\"\n");
        write(
            &pdir.join("config.toml"),
            "[sources]\ncdk_sha = \"86a7c6\"\nnuts_sha = \"3bc8b6\"\n",
        );
        let (remote, tag_sha) = fixture_remote(&root, "v0.17.0");
        write(
            &root.join("sources.toml"),
            &format!(
                "[sources.cdk]\nrepo = \"{remote}\"\ntag = \"v0.17.0\"\nsha = \"{tag_sha}\"\n\
                 [sources.nuts]\nrepo = \"/nonexistent/corpus-nuts-fixture\"\ntag = \"main\"\nsha = \"3bc8b6d5\"\n"
            ),
        );
        std::env::set_var("CORPUS_PLUGINS_DIR", root.join("plugins"));

        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let revs = plugin_sources(&store, "p").unwrap();
        assert_eq!(revs.len(), 2, "config.toml declares cdk + nuts");
        let cdk = revs.iter().find(|s| s.name == "cdk").unwrap();
        assert_eq!(cdk.pinned, "v0.17.0");
        assert_eq!(
            cdk.revs,
            vec!["main".to_string(), "v0.17.0".to_string(), "v0.1.0".to_string()],
            "main first (the default), then the pin, then tags newest-first: {:?}",
            cdk.revs
        );
        assert_eq!(cdk.default_rev(), "main");
        let nuts = revs.iter().find(|s| s.name == "nuts").unwrap();
        assert_eq!(nuts.pinned, "main");
        // nuts' remote is unreachable — degrades to the pin alone.
        assert_eq!(nuts.revs, vec!["main".to_string()]);

        // A project bound to a missing plugin degrades to empty, not error.
        store.create_project("q", "Q", "ghost-plugin").unwrap();
        assert!(plugin_sources(&store, "q").unwrap().is_empty());
        // A missing project errors.
        assert!(plugin_sources(&store, "ghost").is_err());

        // prepare_source_pins: the manifest pin resolves sha-direct and
        // fetches the tree; a branch pin resolves via the rev cache.
        let mut pins = BTreeMap::new();
        pins.insert("cdk".to_string(), "v0.17.0".to_string());
        let resolved = prepare_source_pins(&store, "p", &pins).unwrap();
        assert_eq!(resolved["cdk"], tag_sha);
        assert!(root.join("sources/cdk").join(&tag_sha).join(".git").is_dir());
        pins.insert("cdk".to_string(), "main".to_string());
        let resolved = prepare_source_pins(&store, "p", &pins).unwrap();
        assert_eq!(resolved["cdk"].len(), 40);
        // Unknown revs and undeclared repos: error / ignored.
        pins.insert("cdk".to_string(), "v8.8.8".to_string());
        assert!(prepare_source_pins(&store, "p", &pins).is_err());
        pins.clear();
        pins.insert("ghost".to_string(), "main".to_string());
        assert!(prepare_source_pins(&store, "p", &pins).unwrap().is_empty());

        std::env::remove_var("CORPUS_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A manifest "tag" that is itself a branch (`main`) must NOT freeze
    /// to the recorded setup-time sha: the pin resolves to the LIVE head
    /// via the rev cache. Offline with no cache, the recorded default sha
    /// is the graceful fallback (only for the manifest's own rev).
    #[test]
    fn branch_default_pin_resolves_live_head() {
        let _guard = env_lock();
        let root = std::env::temp_dir().join(format!("corpus-branchpin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pdir = root.join("plugins").join("cdk-regtest");
        write(&pdir.join("plugin.toml"), "name = \"cdk-regtest\"\nexec = \"plugin\"\n");
        write(&pdir.join("config.toml"), "[sources]\nnuts_sha = \"x\"\n");
        let (remote, tag_sha) = fixture_remote(&root, "v0.2.0");
        // The live branch head: after the two fixtures commits, main ==
        // the second commit (== the peeled annotated tag).
        let head = std::process::Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(&remote)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap();
        assert_eq!(head, tag_sha, "fixture main head == annotated tag commit");
        // Manifest tag = "main" with a FABRICATED recorded sha.
        write(
            &root.join("sources.toml"),
            &format!(
                "[sources.nuts]\nrepo = \"{remote}\"\ntag = \"main\"\nsha = \"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\"\n"
            ),
        );
        std::env::set_var("CORPUS_PLUGINS_DIR", root.join("plugins"));
        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "cdk-regtest").unwrap();

        let mut pins = BTreeMap::new();
        pins.insert("nuts".to_string(), "main".to_string());
        let resolved = prepare_source_pins(&store, "p", &pins).unwrap();
        assert_eq!(
            resolved["nuts"], head,
            "branch-valued manifest default resolves the live head, not the freeze"
        );
        assert!(root.join("sources/nuts").join(&head).join(".git").is_dir());

        // Offline fallback: a manifest-default branch with an unreachable
        // remote (no cache) degrades to the recorded default sha — a
        // default must resolve without the network — while a NON-default
        // branch pin on the same source is a loud error.
        write(
            &root.join("sources.toml"),
            &format!(
                "[sources.nuts]\nrepo = \"/nonexistent/corpus-nuts-branch\"\ntag = \"main\"\nsha = \"{tag_sha}\"\n"
            ),
        );
        let mut default = BTreeMap::new();
        default.insert("nuts".to_string(), "main".to_string());
        let resolved = prepare_source_pins(&store, "p", &default).unwrap();
        assert_eq!(resolved["nuts"], tag_sha, "offline default degrades to the audited sha");
        let mut other = BTreeMap::new();
        other.insert("nuts".to_string(), "push".to_string());
        assert!(prepare_source_pins(&store, "p", &other).is_err());

        std::env::remove_var("CORPUS_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }
}
