//! Plugin discovery: a plugins directory holds one subdirectory per
//! plugin, each containing a `plugin.toml` manifest. Probing happens on
//! the host (this module); consumers never spawn plugins themselves.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::Error;
use crate::plugin::{Plugin, ProbeResult};
use crate::store::Store;
pub use corpus_observe::PluginDir;

pub use corpus_store::paths::plugins_dir;

/// Discover all plugins under a directory (invalid entries are skipped
/// with a warning on stderr, so one bad plugin can't break the registry).
pub fn discover(dir: &Path) -> Result<Vec<PluginDir>, Error> {
    corpus_observe::discover_plugins(dir)
}

/// Effective installed + transitional bundled catalog.
pub fn plugin_catalog() -> Result<Vec<PluginDir>, Error> {
    corpus_observe::plugin_catalog()
}

pub fn find_plugin(name: &str) -> Result<Option<PluginDir>, Error> {
    corpus_observe::catalog_plugin(name)
}

/// A discovered plugin plus its live probe status — the aggregation the
/// app renders. spawned/probed on the host inside corpus-core, so the
/// UI never spawns plugins itself (trust domains).
#[derive(Debug, Clone)]
pub struct PluginStatus {
    /// Plugin name (unique within the registry).
    pub name: String,
    /// The PLUGIN's own manifest version — NOT the target version. See
    /// `running_version` for what the environment is actually running.
    pub version: Option<String>,
    pub description: Option<String>,
    /// Whether this entry has a live probe result. Discovery and probing are
    /// separate so the app can list every binding without spawning every
    /// plugin process.
    pub probed: bool,
    /// Environment readiness from a live probe.
    pub ready: bool,
    /// Human-readable detail (what is missing, versions, etc.).
    pub notes: String,
    /// The version the TARGET is actually running right now (from the live
    /// probe), e.g. the mint's reported version. `None` when unreachable.
    pub running_version: Option<String>,
    /// The rev name the manifest expects to be running (the probe's view of
    /// the `sources.toml` tag).
    pub expected_tag: Option<String>,
}

/// Discover every plugin and probe each one live. Failures are folded
/// into `ready: false` + a note, so one broken plugin can't abort the
/// aggregation.
pub fn plugin_status() -> Vec<PluginStatus> {
    plugin_status_for(None, true)
}

/// Discover every plugin but live-probe only `selected`. The app uses this
/// path so changing projects starts one environment process, not one per
/// installed plugin. `probe_all` is retained for operator/admin diagnostics.
pub fn selected_plugin_status(selected: Option<&str>) -> Vec<PluginStatus> {
    plugin_status_for(selected, false)
}

fn plugin_status_for(selected: Option<&str>, probe_all: bool) -> Vec<PluginStatus> {
    let mut out = Vec::new();
    for plugin in plugin_catalog().unwrap_or_default() {
        let should_probe = probe_all || selected == Some(plugin.manifest.name.as_str());
        // Keep the whole ProbeResult so the version fields survive — a
        // `(ready, notes)` destructure was what dropped them before.
        let probe = if should_probe {
            match Plugin::spawn(&plugin.dir) {
                Ok(mut spawned) => {
                    let result = if plugin.manifest.manifest_version
                        == corpus_observe::PluginManifestVersion::V1
                    {
                        v1_status(&plugin, &mut spawned)
                    } else {
                        spawned.probe()
                    };
                    result.unwrap_or_else(|error| ProbeResult {
                        ready: false,
                        notes: format!("probe failed: {error}"),
                        running_version: None,
                        expected_tag: None,
                    })
                }
                Err(error) => ProbeResult {
                    ready: false,
                    notes: format!("spawn failed: {error}"),
                    running_version: None,
                    expected_tag: None,
                },
            }
        } else {
            ProbeResult {
                ready: false,
                notes: "not probed".into(),
                running_version: None,
                expected_tag: None,
            }
        };
        out.push(PluginStatus {
            name: plugin.manifest.name.clone(),
            version: plugin.manifest.version.clone(),
            description: plugin.manifest.description.clone(),
            probed: should_probe,
            ready: probe.ready,
            notes: probe.notes,
            running_version: probe.running_version,
            expected_tag: probe.expected_tag,
        });
    }
    out
}

fn v1_status(plugin: &PluginDir, spawned: &mut Plugin) -> Result<ProbeResult, Error> {
    spawned.hello()?;
    let params = crate::plugin_lifecycle_params(plugin)?;
    let result = spawned.lifecycle_call(
        "status",
        Some(params),
        std::time::Duration::from_secs(10),
        |_| {},
    )?;
    Ok(ProbeResult {
        ready: result
            .get("ready")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        notes: result
            .get("notes")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| result.to_string()),
        running_version: result
            .get("running_version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        expected_tag: result
            .get("expected_tag")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
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

/// The source repos a project's plugin declares in its v1 manifest (legacy
/// plugins retain their temporary `config.toml` + root-manifest adapter), with the revisions the top
/// bar may offer: the manifest pin plus the live remote tag set (cached
/// under `sources/.rev-cache/`). Returns an empty vec (never an error)
/// when the project's plugin or its `config.toml`/`[sources]` can't be
/// found — the top bar then keeps its placeholder pins rather than
/// failing. A missing project is an error.
pub fn plugin_sources(store: &Store, project: &str) -> Result<Vec<SourceRevs>, Error> {
    let spec = crate::store::Project::load(store, project)?;
    let Some(pdir) = find_plugin(&spec.plugin)?
    else {
        return Ok(Vec::new());
    };
    let entries = source_entries_for_plugin(&pdir, &spec.plugin)?;
    let sources_dir = store.source_cache_dir();

    let mut out = Vec::new();
    for (repo, entry) in entries {
        let pinned = entry.tag.clone();
        let revs = crate::srcrev::selectable_revs(&sources_dir, &repo, &entry.repo, &pinned);
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

/// Validate ONE author-time pin `(name, rev)` structurally — NO network.
///
/// The pin surfaces that accept free text (the curator's `mission_new` /
/// `mission_set_pins`, the CLI `--pin`) call this so a rev that could never
/// resolve is rejected at authoring, with a clear error, instead of a
/// silent time-bomb that only detonates at launch (deep in
/// `prepare_source_pins`). It deliberately does not `ls-remote`: it checks
/// the rev against the DISK rev cache only, so it stays fast and offline-
/// safe. Real resolution + fetch still happens at launch.
///
/// A rev is accepted when it is (a) the manifest tag, (b) a rev in the
/// source's selectable set, (c) `main`/`master`, or (d) a 40-hex commit
/// sha. FAIL-OPEN when the source name is not among the project's declared
/// sources — mirrors `prepare_source_pins` ignoring undeclared repos, and
/// keeps test rigs (no discoverable plugin) working.
pub fn validate_pin(store: &Store, project: &str, name: &str, rev: &str) -> Result<(), Error> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err(Error::Store(format!("pin {name}: rev is empty")));
    }
    // Fail-open on any inability to enumerate the source set (plugin not
    // discoverable, no [sources] table, etc.): validation is best-effort —
    // launch still resolves for real. We only REJECT when we positively
    // know the rev set and the pin is not in it.
    let Ok(sources) = plugin_sources(store, project) else {
        return Ok(());
    };
    let Some(source) = sources.iter().find(|s| s.name == name) else {
        return Ok(()); // undeclared source: not this check's to reject
    };
    let ok = rev == source.pinned
        || source.revs.iter().any(|r| r == rev)
        || matches!(rev, "main" | "master")
        || crate::srcrev::is_commit_sha(rev);
    if ok {
        Ok(())
    } else {
        Err(Error::Store(format!(
            "pin {name}={rev:?} is not a known rev, tag, main/master, or a 40-hex commit sha — known: {}",
            source.revs.join(", ")
        )))
    }
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
    let Some(pdir) = find_plugin(&spec.plugin)?
    else {
        return Err(Error::Store(format!(
            "mission carries source pins but plugin {} is not discovered",
            spec.plugin
        )));
    };
    let entries = source_entries_for_plugin(&pdir, &spec.plugin)?;
    let sources_dir = store.source_cache_dir();
    for (name, rev) in pins {
        if rev.trim().is_empty() || !entries.contains_key(name) {
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
        let sha = if crate::srcrev::is_commit_sha(rev) {
            // A pin that IS a commit sha needs no name resolution — it is
            // already the sha. `ensure_source_tree` fetches it directly.
            rev.clone()
        } else if rev == &entry.tag && !entry.sha.is_empty() && !branch_default {
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

#[derive(Clone, Deserialize)]
struct SourceEntry {
    #[serde(default)]
    repo: String,
    tag: String,
    /// The manifest pin's sha — the offline-runnable default resolution.
    #[serde(default)]
    sha: String,
}

fn source_entries_for_plugin(
    plugin: &PluginDir,
    plugin_name: &str,
) -> Result<BTreeMap<String, SourceEntry>, Error> {
    if plugin.manifest.manifest_version == corpus_observe::PluginManifestVersion::V1 {
        return Ok(plugin
            .manifest
            .sources
            .iter()
            .map(|source| {
                (
                    source.id.clone(),
                    SourceEntry {
                        repo: source.repo.clone(),
                        tag: source.default_rev.clone(),
                        sha: source.default_sha.clone(),
                    },
                )
            })
            .collect());
    }

    let raw = fs::read_to_string(plugin.dir.join("config.toml"))
        .map_err(|error| Error::Store(format!("plugin {plugin_name} config.toml: {error}")))?;
    let config: toml::Value = toml::from_str(&raw)
        .map_err(|error| Error::Store(format!("plugin {plugin_name} config.toml: {error}")))?;
    let declared: Vec<String> = config
        .get("sources")
        .and_then(toml::Value::as_table)
        .map(|table| {
            table
                .keys()
                .filter(|key| key.ends_with("_sha"))
                .map(|key| key.trim_end_matches("_sha").to_string())
                .collect()
        })
        .unwrap_or_default();
    let manifest = std::env::var(crate::paths::PLUGINS_DIR_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .and_then(|root| Path::new(&root).parent().map(|parent| parent.join("sources.toml")))
        .unwrap_or(crate::paths::sources_manifest()?);
    let entries = load_source_entries(&manifest);
    Ok(entries
        .into_iter()
        .filter(|(name, _)| declared.contains(name))
        .collect())
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
    use crate::test_support::{env_lock, unique_temp_path, EnvVarGuard};

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
        // Let a bare-sha fetch work from this LOCAL fixture the way GitHub
        // serves one (the sha-direct path in ensure_source_tree).
        run(&["config", "uploadpack.allowAnySHA1InWant", "true"]);
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
        let root = unique_temp_path("corpus-plugin-src");
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
        let _plugins = EnvVarGuard::set("CORPUS_PLUGINS_DIR", root.join("plugins"));
        let _sources = EnvVarGuard::set("CORPUS_SOURCES_DIR", root.join("sources"));

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

        // A raw commit SHA pins DIRECTLY — no name resolution (the failure
        // mode this fixes), fetched by sha into sources/cdk/<sha>/.
        pins.clear();
        pins.insert("cdk".to_string(), tag_sha.clone());
        let resolved = prepare_source_pins(&store, "p", &pins).unwrap();
        assert_eq!(resolved["cdk"], tag_sha);
        assert!(root.join("sources/cdk").join(&tag_sha).join(".git").is_dir());

        // validate_pin (author-time, structural): the manifest tag, main, a
        // selectable tag, and a 40-hex sha all pass; a typo fails; an
        // undeclared source is fail-open (mirrors prepare_source_pins).
        assert!(validate_pin(&store, "p", "cdk", "v0.17.0").is_ok());
        assert!(validate_pin(&store, "p", "cdk", "main").is_ok());
        assert!(validate_pin(&store, "p", "cdk", "v0.1.0").is_ok());
        assert!(validate_pin(&store, "p", "cdk", &tag_sha).is_ok());
        assert!(validate_pin(&store, "p", "cdk", "v9.9.9").is_err());
        assert!(validate_pin(&store, "p", "ghost", "anything").is_ok());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A manifest "tag" that is itself a branch (`main`) must NOT freeze
    /// to the recorded setup-time sha: the pin resolves to the LIVE head
    /// via the rev cache. Offline with no cache, the recorded default sha
    /// is the graceful fallback (only for the manifest's own rev).
    #[test]
    fn branch_default_pin_resolves_live_head() {
        let _guard = env_lock();
        let root = unique_temp_path("corpus-branchpin");
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
        let _plugins = EnvVarGuard::set("CORPUS_PLUGINS_DIR", root.join("plugins"));
        let _sources = EnvVarGuard::set("CORPUS_SOURCES_DIR", root.join("sources"));
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

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v1_manifest_owns_source_declarations_and_cache_custody() {
        let _guard = env_lock();
        let root = unique_temp_path("corpus-v1-source");
        let _ = std::fs::remove_dir_all(&root);
        let (remote, sha) = fixture_remote(&root, "v1.0.0");
        let plugin = root.join("catalog").join("fixture-regtest");
        write(
            &plugin.join("plugin.toml"),
            &format!(
                "manifest_version = 1\nid = \"fixture-regtest\"\nversion = \"1.0.0\"\nprotocol = \"corpus.environment/1\"\nexec = \"plugin\"\n\n[[sources]]\nid = \"target\"\nrepo = \"{remote}\"\ndefault_rev = \"v1.0.0\"\ndefault_sha = \"{sha}\"\nmount = \"/opt/src/target\"\n"
            ),
        );
        write(&plugin.join("plugin"), "#!/bin/sh\nexit 0\n");
        let _plugins = EnvVarGuard::set("CORPUS_PLUGINS_DIR", root.join("catalog"));
        let store = Store::new(root.join("data/store"));
        store.create_project("p", "P", "fixture-regtest").unwrap();

        let sources = plugin_sources(&store, "p").unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "target");
        assert_eq!(sources[0].pinned, "v1.0.0");
        assert!(!plugin.join("config.toml").exists());
        assert!(!root.join("sources.toml").exists());

        let mut pins = BTreeMap::new();
        pins.insert("target".to_string(), "v1.0.0".to_string());
        let resolved = prepare_source_pins(&store, "p", &pins).unwrap();
        assert_eq!(resolved["target"], sha);
        assert!(store.source_cache_dir().join("target").join(&sha).join(".git").is_dir());

        let run = store.provision_run_dir("p").unwrap();
        assert_eq!(fs::read_link(run.join("sources")).unwrap(), store.source_cache_dir());
        let _ = std::fs::remove_dir_all(&root);
    }
}
