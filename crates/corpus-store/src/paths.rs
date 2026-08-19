//! Canonical corpus data and resource paths.
//!
//! corpus has TWO roots and they are not the same thing:
//!
//! - the **data root** (`~/.corpus`) — everything the operator produces:
//!   projects, corpora, missions, run dirs, chat scopes, app prefs. Owned
//!   by the user, survives a reinstall, never checked into a repo.
//! - the **resource root** — optional assets shipped WITH the app: model
//!   metadata and skills. Production plugins
//!   and fetched sources live under the data root instead. Read-only at
//!   runtime, replaced wholesale by an upgrade.
//!
//! They used to be one: the store lived at `<repo>/store`, and four call
//! sites recovered "the repo" as `store.root().parent()`. That coupling is
//! what put the opencode run cwd INSIDE the git repo, where opencode
//! discovers the repo-root `.opencode/` — and with it another project's
//! agents. Splitting the roots is what moves the run dir out of the repo;
//! `store.root().parent()` is deliberately gone.
//!
//! Every resolver here is pure path computation. Nothing creates
//! directories; `Store::provision_run_dir` and `create_project` own that.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Override for the data root (projects, runs, chat, prefs).
pub const HOME_ENV: &str = "CORPUS_HOME";
/// Override for the store root specifically. Wins over [`HOME_ENV`], and
/// relocates the run and chat roots with it — one variable moves a whole
/// world, which is what the tests rely on.
pub const STORE_ENV: &str = "CORPUS_STORE";
/// Override for the optional shipped-asset root.
pub const RESOURCES_ENV: &str = "CORPUS_RESOURCES";
/// Environment variable overriding the installed plugin catalog directory.
pub const PLUGINS_DIR_ENV: &str = "CORPUS_PLUGINS_DIR";
/// Override for the corpus-owned pinned source cache.
pub const SOURCES_DIR_ENV: &str = "CORPUS_SOURCES_DIR";
/// Override for the optional benchmark/model metadata registry.
pub const MODELS_ENV: &str = "CORPUS_MODELS";

/// The data root: `CORPUS_HOME`, else `~/.corpus`.
pub fn data_root() -> PathBuf {
    if let Some(dir) = std::env::var(HOME_ENV).ok().filter(|s| !s.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".corpus")
}

/// The store root: `CORPUS_STORE`, else `<data root>/store`.
pub fn store_root() -> PathBuf {
    match std::env::var(STORE_ENV).ok().filter(|s| !s.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => data_root().join("store"),
    }
}

// Run directories (`<store parent>/var/run/<project>`) and chat scopes
// (`.../var/chat/<project>`) are computed by `Store` — see
// `Store::project_run_dir` and `Store::project_chat_dir`. They deliberately
// hang off the store INSTANCE rather than the environment: a `Store` built
// over a temp dir must keep its runs there, and an env-derived answer sent
// every test's run dir into the real `~/.corpus`.

/// A directory is the resource root if it carries a shipped resource.
/// Installed plugins and fetched sources deliberately are not markers: a
/// clean corpus build must resolve its optional assets before either exists.
fn is_resource_root(dir: &Path) -> bool {
    dir.join("benchmarks/models.yaml").is_file()
        || dir.join(".opencode/skills").is_dir()
}

static RESOURCE_ROOT: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();

/// The resource root, resolved once per process.
///
/// Order: `CORPUS_RESOURCES` (loud if it fails the marker — an explicit
/// wrong answer must never fall through to a guess), then the running
/// executable's ancestors (covers `target/debug/`, `target/debug/deps/`,
/// and a macOS `.app` bundle's `Resources`), then a build-time bake for
/// packaging, then — debug builds only — the source tree, so `cargo run`
/// and `cargo test` need no configuration at all, then the cwd's
/// ancestors.
pub fn resource_root() -> Result<PathBuf> {
    RESOURCE_ROOT
        .get_or_init(resolve_resource_root)
        .clone()
        .map_err(Error::Store)
}

/// The resource root if there is one. For callers that degrade instead of
/// failing (a store-only CLI subcommand has no use for `sources/`).
pub fn resource_root_opt() -> Option<PathBuf> {
    resource_root().ok()
}

fn resolve_resource_root() -> std::result::Result<PathBuf, String> {
    let mut tried: Vec<String> = Vec::new();
    let check = |dir: PathBuf, tried: &mut Vec<String>| -> Option<PathBuf> {
        let ok = is_resource_root(&dir);
        tried.push(format!(
            "{}{}",
            dir.display(),
            if ok { " (ok)" } else { "" }
        ));
        ok.then_some(dir)
    };

    if let Some(dir) = std::env::var(RESOURCES_ENV).ok().filter(|s| !s.is_empty()) {
        let dir = PathBuf::from(dir);
        return match is_resource_root(&dir) {
            true => Ok(dir),
            // Explicit and wrong: say so instead of quietly resolving
            // somewhere else, which is how you debug the wrong tree.
            false => Err(format!(
                "{RESOURCES_ENV}={} is not a corpus resource root (no shipped corpus assets found)",
                dir.display()
            )),
        };
    }

    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(4) {
            if let Some(dir) = check(ancestor.to_path_buf(), &mut tried) {
                return Ok(dir);
            }
            if let Some(dir) = check(ancestor.join("Resources"), &mut tried) {
                return Ok(dir);
            }
        }
    }

    if let Some(baked) = option_env!("CORPUS_RESOURCES_DIR") {
        if let Some(dir) = check(PathBuf::from(baked), &mut tried) {
            return Ok(dir);
        }
    }

    // Development: the workspace this binary was built from.
    if cfg!(debug_assertions) {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(dir) = manifest.parent().and_then(Path::parent) {
            if let Some(dir) = check(dir.to_path_buf(), &mut tried) {
                return Ok(dir);
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors().take(6) {
            if let Some(dir) = check(ancestor.to_path_buf(), &mut tried) {
                return Ok(dir);
            }
        }
    }

    Err(format!(
        "corpus resources not found — tried: {}. Set {RESOURCES_ENV} to the directory holding \
         shipped corpus assets.",
        tried.join(", ")
    ))
}

/// Writable root for versioned plugin installations.
pub fn plugin_install_root() -> PathBuf {
    data_root().join("plugins")
}

/// Writable runtime state root. Installed bundles remain read-only.
pub fn plugin_runtime_root() -> PathBuf {
    data_root().join("var/plugins")
}

/// Resolve the primary plugin catalog directory. An explicit override is a
/// complete development/test catalog; otherwise this is the writable install
/// root.
pub fn plugins_dir() -> PathBuf {
    if let Some(dir) = std::env::var(PLUGINS_DIR_ENV)
        .ok()
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(dir);
    }
    plugin_install_root()
}

/// The corpus-owned pinned-source cache (`cache/sources/<name>/<sha>/`).
pub fn sources_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var(SOURCES_DIR_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(dir));
    }
    Ok(data_root().join("cache/sources"))
}

/// The optional model metadata registry shipped under the resource root.
/// An explicit override is accepted even when the file does not exist;
/// `ModelRegistry::load` owns the documented empty-registry degradation.
pub fn models_manifest() -> Result<PathBuf> {
    if let Some(path) = std::env::var(MODELS_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    Ok(resource_root()?.join("benchmarks/models.yaml"))
}

/// The corpus-mcp binary a generated opencode config should spawn.
/// `CORPUS_MCP` overrides; otherwise it sits beside the running executable
/// (the app and the CLI ship next to it), one level up from a test binary
/// in `deps/`, or under the resource root for a packaged install.
pub fn corpus_mcp_bin() -> Result<PathBuf> {
    companion_bin("corpus-mcp", "CORPUS_MCP")
}

/// The host-side admin MCP binary used by management chat.
/// `CORPUS_ADMIN_MCP` overrides the packaged/sibling lookup.
pub fn corpus_admin_mcp_bin() -> Result<PathBuf> {
    companion_bin("corpus-admin-mcp", "CORPUS_ADMIN_MCP")
}

fn companion_bin(name: &str, env: &str) -> Result<PathBuf> {
    if let Some(explicit) = std::env::var(env).ok().filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(explicit));
    }
    let mut tried: Vec<String> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [dir.join(name), dir.join("..").join(name)] {
                if candidate.is_file() {
                    // Canonicalized: this path is written verbatim into
                    // every generated opencode.json, where a `..` segment
                    // is just noise the operator has to decode.
                    return Ok(candidate.canonicalize().unwrap_or(candidate));
                }
                tried.push(candidate.display().to_string());
            }
        }
    }
    if let Some(root) = resource_root_opt() {
        for candidate in [
            root.join("bin").join(name),
            root.join("target/debug").join(name),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
            tried.push(candidate.display().to_string());
        }
    }
    Err(Error::Store(format!(
        "{name} binary not found — tried: {}. Set {env} to its path.",
        tried.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An explicitly-set resource root that is wrong must FAIL, not fall
    /// through to a guess: silently resolving somewhere else is how a run
    /// ends up reading a different source tree than the operator set.
    #[test]
    fn an_explicit_resource_root_must_be_real() {
        let dir = std::env::temp_dir().join(format!("corpus-noresource-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_resource_root(&dir));
        // Resolution is cached per process, so exercise the resolver's
        // marker directly rather than racing the OnceLock.
        assert!(is_resource_root(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
        ));
        let clean = dir.join("clean-install");
        std::fs::create_dir_all(clean.join("benchmarks")).unwrap();
        std::fs::write(clean.join("benchmarks/models.yaml"), "models: []\n").unwrap();
        assert!(
            is_resource_root(&clean),
            "resource discovery must not require plugins/ or sources.toml"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The data root is `~/.corpus` unless told otherwise, and the store
    /// sits inside it — so `var/run` and `var/chat` are siblings of
    /// `store/`, never inside a project (a run dir inside the project it
    /// links would make that link a cycle).
    #[test]
    fn the_store_lives_under_the_data_root() {
        // Deliberately does NOT set or clear the environment: these
        // variables are process-global and several launch tests steer them
        // under their own lock, so a mutation here reaches across threads
        // and fails an unrelated test.
        match std::env::var(STORE_ENV).ok().filter(|s| !s.is_empty()) {
            Some(explicit) => assert_eq!(store_root(), PathBuf::from(explicit)),
            None => {
                assert_eq!(store_root(), data_root().join("store"));
                if std::env::var(HOME_ENV).is_err() {
                    assert!(data_root().ends_with(".corpus"));
                }
            }
        }
    }
}
