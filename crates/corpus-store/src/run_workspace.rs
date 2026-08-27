//! Project-scoped run-workspace paths and materialization.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::filesystem::atomic_write;
use crate::projects::Project;
use crate::store::{Store, PROJECT_ENV, STORE_ENV};

const SOURCE_VIEW_PREFIX: &str = "sources-";

/// Durable, relocatable identity of one exact OpenCode working directory.
/// The id is persisted with a mission; the absolute path is always rebuilt
/// beneath that mission's project run root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunWorkspace {
    pub id: String,
    pub path: PathBuf,
}

impl Store {
    /// Corpus-owned source cache paired with this store instance. Tests and
    /// alternate stores stay in their own world instead of touching the
    /// operator's default home.
    pub fn source_cache_dir(&self) -> PathBuf {
        if let Some(explicit) = std::env::var(crate::paths::SOURCES_DIR_ENV)
            .ok()
            .filter(|value| !value.is_empty())
        {
            return PathBuf::from(explicit);
        }
        self.mutable_root().join("cache/sources")
    }

    /// This store's mutable side-tree (`<store parent>/var`).
    pub fn var_dir(&self) -> PathBuf {
        self.mutable_root().join("var")
    }

    /// The project's run root. OpenCode launches work in an exact child of
    /// `views/`; this root itself is only a staging and namespace boundary.
    pub fn project_run_dir(&self, slug: &str) -> PathBuf {
        self.var_dir().join("run").join(slug)
    }

    /// This store's Goose management-chat scope for a project.
    pub fn project_chat_dir(&self, slug: &str) -> PathBuf {
        self.var_dir().join("chat").join(slug)
    }

    /// Materialize a launch workspace with no source trees exposed.
    pub fn provision_run_dir(&self, slug: &str) -> Result<PathBuf> {
        self.provision_run_dir_with_sources(slug, None)
    }

    /// Resolve a persisted workspace id beneath one project's run root.
    /// Stored values are never accepted as paths.
    pub fn run_workspace_dir(&self, project: &str, workspace: &str) -> Result<PathBuf> {
        crate::store::validate_slug(project)?;
        validate_workspace_id(workspace)?;
        let project_run = self.project_run_dir(project);
        existing_real_dir(&project_run)?;
        let views = project_run.join("views");
        existing_real_dir(&views)?;
        let workspace = views.join(workspace);
        existing_real_dir(&workspace)?;
        Ok(workspace)
    }

    /// Enumerate only Corpus-shaped, real workspace directories for legacy
    /// session recovery. Invalid names and symlinks are never candidates.
    pub fn run_workspaces(&self, project: &str) -> Result<Vec<RunWorkspace>> {
        crate::store::validate_slug(project)?;
        let project_run = self.project_run_dir(project);
        if !existing_real_dir(&project_run)? {
            return Ok(Vec::new());
        }
        let views = project_run.join("views");
        if !existing_real_dir(&views)? {
            return Ok(Vec::new());
        }
        let mut workspaces = Vec::new();
        for entry in fs::read_dir(views)? {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if validate_workspace_id(&id).is_err() {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                workspaces.push(RunWorkspace {
                    id,
                    path: entry.path(),
                });
            }
        }
        workspaces.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(workspaces)
    }

    /// Materialize the directory boundary for one project's exact source
    /// pins. Views are immutable-by-identity and separate concurrent launches
    /// whose pins differ, while the fetched cache remains host-only.
    ///
    /// The workspace exposes only the selected project and exact pinned trees;
    /// it deliberately lives beside the store so linking the project into the
    /// workspace cannot form a recursive directory cycle.
    pub fn provision_run_dir_with_sources(
        &self,
        slug: &str,
        source_pins_json: Option<&str>,
    ) -> Result<PathBuf> {
        self.provision_run_workspace_with_sources(slug, source_pins_json)
            .map(|workspace| workspace.path)
    }

    /// Materialize and identify the exact launch workspace. Callers that
    /// create durable run/session state must retain the returned id.
    pub fn provision_run_workspace_with_sources(
        &self,
        slug: &str,
        source_pins_json: Option<&str>,
    ) -> Result<RunWorkspace> {
        Project::load(self, slug)?;
        let pins = parse_source_pins(source_pins_json)?;
        let var_dir = self.var_dir();
        ensure_real_dir(&var_dir)?;
        let run_base = var_dir.join("run");
        ensure_real_dir(&run_base)?;
        let project_run = run_base.join(slug);
        ensure_real_dir(&project_run)?;
        // Agent rendering remains a project-stable staging operation. Each
        // pin-specific view receives a snapshot below.
        let views = project_run.join("views");
        ensure_real_dir(&views)?;
        let workspace_id = source_view_key(&pins);
        let run_dir = views.join(&workspace_id);
        ensure_real_dir(&run_dir)?;
        let opencode = run_dir.join(".opencode");
        ensure_real_dir(&opencode)?;
        ensure_real_dir(&opencode.join("agent"))?;
        sync_rendered_agents(
            &project_run.join(".opencode/agent"),
            &opencode.join("agent"),
        )?;
        let store_dir = run_dir.join("store");
        ensure_real_dir(&store_dir)?;
        let projects_dir = store_dir.join("projects");
        ensure_real_dir(&projects_dir)?;
        ensure_single_project_namespace(&projects_dir, slug)?;

        relink(&self.project_dir(slug), &projects_dir.join(slug))?;

        let sources = run_dir.join("sources");
        ensure_real_dir(&sources)?;
        provision_source_view(self, &sources, &pins)?;

        // Skills are optional because not every installation ships them.
        if let Some(resources) = crate::paths::resource_root_opt() {
            let skills = resources.join(".opencode").join("skills");
            if skills.exists() {
                relink(&skills, &opencode.join("skills"))?;
            }
        }

        write_run_opencode_config(self, slug, &opencode)?;
        Ok(RunWorkspace {
            id: workspace_id,
            path: run_dir,
        })
    }
}

fn validate_workspace_id(workspace: &str) -> Result<()> {
    let digest = workspace
        .strip_prefix(SOURCE_VIEW_PREFIX)
        .filter(|digest| digest.len() == 64)
        .filter(|digest| {
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| Error::Store(format!("invalid run workspace id: {workspace:?}")))?;
    debug_assert_eq!(digest.len(), 64);
    Ok(())
}

fn existing_real_dir(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(Error::Store(format!(
            "run workspace boundary is not a real directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn parse_source_pins(raw: Option<&str>) -> Result<std::collections::BTreeMap<String, String>> {
    let Some(raw) = raw else {
        return Ok(std::collections::BTreeMap::new());
    };
    let pins: std::collections::BTreeMap<String, String> = serde_json::from_str(raw)
        .map_err(|error| Error::Store(format!("source pins are not a string map: {error}")))?;
    for (source, sha) in &pins {
        if source.is_empty()
            || source.len() > 64
            || !source
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(Error::Store(format!(
                "invalid source id in launch pins: {source:?}"
            )));
        }
        if sha.len() != 40
            || !sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::Store(format!(
                "invalid source sha for {source}: {sha:?}"
            )));
        }
    }
    Ok(pins)
}

fn source_view_key(pins: &std::collections::BTreeMap<String, String>) -> String {
    let canonical = serde_json::to_vec(pins).expect("a string map always serializes");
    let digest = Sha256::digest(canonical);
    format!("sources-{:x}", digest)
}

fn provision_source_view(
    store: &Store,
    view: &Path,
    pins: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    ensure_exact_names(view, pins.keys().map(String::as_str))?;
    let cache = store.source_cache_dir();
    fs::create_dir_all(&cache)?;
    let cache = cache.canonicalize()?;
    for (source, sha) in pins {
        let source_view = view.join(source);
        ensure_real_dir(&source_view)?;
        ensure_exact_names(&source_view, std::iter::once(sha.as_str()))?;
        let target = cache.join(source).join(sha);
        let target = target.canonicalize().map_err(|error| {
            Error::Store(format!(
                "pinned source {source}@{sha} is unavailable: {error}"
            ))
        })?;
        if !target.starts_with(&cache) {
            return Err(Error::Store(format!(
                "pinned source {source}@{sha} resolves outside the source cache"
            )));
        }
        validate_regular_tree(&target, "pinned source")?;
        relink(&target, &source_view.join(sha))?;
    }
    Ok(())
}

fn ensure_exact_names<'a>(dir: &Path, expected: impl Iterator<Item = &'a str>) -> Result<()> {
    let expected: std::collections::BTreeSet<&str> = expected.collect();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::Store(format!(
                "run source view contains a non-UTF-8 entry: {}",
                entry.path().display()
            )));
        };
        if !expected.contains(name) {
            return Err(Error::Store(format!(
                "run source view {} contains unexpected entry {}",
                dir.display(),
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn validate_regular_tree(root: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::Store(format!(
            "{label} root is not a real directory: {}",
            root.display()
        )));
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
                return Err(Error::Store(format!(
                    "{label} contains a symlink or special file: {}",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(())
}

fn sync_rendered_agents(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(Error::Store(format!(
                    "run agent destination contains an unsupported entry: {}",
                    path.display()
                )));
            }
            fs::remove_file(path)?;
        }
    }
    if !source.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Store(format!(
                "rendered agent is not a regular file: {}",
                path.display()
            )));
        }
        let contents = fs::read(&path)?;
        atomic_write(&destination.join(entry.file_name()), contents)?;
    }
    Ok(())
}

/// Create one corpus-managed directory component without following an
/// existing symlink or accepting another filesystem object in its place.
fn ensure_real_dir(path: &Path) -> Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(Error::Store(format!(
            "{} is not a real directory — refusing to provision through it",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// The workspace project namespace is capability-defining: even a stale
/// second link would make another project's data reachable from this run.
fn ensure_single_project_namespace(projects_dir: &Path, slug: &str) -> Result<()> {
    for entry in fs::read_dir(projects_dir)? {
        let entry = entry?;
        if entry.file_name() != std::ffi::OsStr::new(slug) {
            return Err(Error::Store(format!(
                "run workspace {} contains unexpected project entry {}",
                projects_dir.display(),
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// Write project scope into the workspace's real, per-project OpenCode config.
/// Per-run agent, source-pin, and transcript identities intentionally remain
/// launch-time environment rather than durable project configuration.
fn write_run_opencode_config(store: &Store, slug: &str, opencode: &Path) -> Result<()> {
    let mcp_bin = crate::paths::corpus_mcp_bin()?;
    let mut mcp_environment = serde_json::Map::from_iter([
        (
            STORE_ENV.to_string(),
            serde_json::Value::String(store.root().to_string_lossy().into_owned()),
        ),
        (
            PROJECT_ENV.to_string(),
            serde_json::Value::String(slug.to_string()),
        ),
    ]);
    // OpenCode may be launched inside an already-running tmux server, whose
    // inherited environment does not include later process-local overrides.
    // Put the documented development/test catalog override on the local MCP
    // definition explicitly so missions and the app resolve the same plugin.
    if let Some(plugins_dir) = std::env::var(crate::paths::PLUGINS_DIR_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        mcp_environment.insert(
            crate::paths::PLUGINS_DIR_ENV.to_string(),
            serde_json::Value::String(plugins_dir),
        );
    }
    let config = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": {
            "corpus": {
                "type": "local",
                "enabled": true,
                "timeout": 180000,
                "command": [mcp_bin.to_string_lossy()],
                "environment": mcp_environment
            }
        }
    });
    let body = serde_json::to_string_pretty(&config)
        .map_err(|error| Error::Store(format!("run config: {error}")))?;
    atomic_write(&opencode.join("opencode.json"), body + "\n")
}

/// Point `link` at `target`, repairing a stale symlink but refusing to
/// overwrite a real path at the workspace boundary.
#[cfg(unix)]
fn relink(target: &Path, link: &Path) -> Result<()> {
    if !target.exists() {
        return Err(Error::Store(format!(
            "cannot link {}: it does not exist",
            target.display()
        )));
    }
    match link.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            if fs::read_link(link).ok().as_deref() == Some(target) {
                return Ok(());
            }
            fs::remove_file(link)?;
        }
        Ok(_) => {
            return Err(Error::Store(format!(
                "{} is a real path, not a link to {} — refusing to provision over it",
                link.display(),
                target.display()
            )));
        }
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn relink(_target: &Path, _link: &Path) -> Result<()> {
    Err(Error::Store(
        "run directories need symlinks; this platform has none".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_records::RUN_LOG_ENV;

    #[test]
    fn persisted_workspace_ids_resolve_only_beneath_the_project_views_root() {
        let store = tmp_store("workspace-id");
        store.create_project("p", "P", "fixture").unwrap();
        let workspace = store
            .provision_run_workspace_with_sources("p", None)
            .unwrap();

        assert_eq!(
            store.run_workspace_dir("p", &workspace.id).unwrap(),
            workspace.path
        );
        assert!(workspace.id.starts_with(SOURCE_VIEW_PREFIX));
        for invalid in [
            "sources-short",
            "../sources-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "/tmp/sources-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sources-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(store.run_workspace_dir("p", invalid).is_err(), "{invalid}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            fs::remove_dir_all(&workspace.path).unwrap();
            symlink(std::env::temp_dir(), &workspace.path).unwrap();
            assert!(store.run_workspace_dir("p", &workspace.id).is_err());
            assert!(store.run_workspaces("p").unwrap().is_empty());
        }
        let _ = fs::remove_dir_all(store.root());
    }
    use crate::store::AGENT_ENV;

    fn tmp_store(tag: &str) -> Store {
        let world =
            std::env::temp_dir().join(format!("corpus-run-workspace-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    #[test]
    fn a_run_dir_exposes_only_its_own_project() {
        let store = tmp_store("scope");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        store.create_project("b", "B", "cdk-regtest").unwrap();
        let run = store.provision_run_dir("a").unwrap();

        let projects = run.join("store/projects");
        let visible: Vec<String> = fs::read_dir(&projects)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(visible, ["a"], "only its own project");
        assert!(!projects.join("b").exists(), "project b is not reachable");
        assert_eq!(
            fs::read_link(projects.join("a")).unwrap(),
            store.project_dir("a")
        );
        assert!(!run.starts_with(store.project_dir("a")));
        assert!(!run.join("benchmarks").exists());
        assert!(!run.join("plugins").exists());
    }

    #[test]
    fn provisioning_repoints_a_stale_link() {
        let store = tmp_store("stale");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        store.create_project("b", "B", "cdk-regtest").unwrap();
        let run = store.provision_run_dir("a").unwrap();
        let link = run.join("store/projects/a");

        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(store.project_dir("b"), &link).unwrap();
        store.provision_run_dir("a").unwrap();
        assert_eq!(fs::read_link(link).unwrap(), store.project_dir("a"));
    }

    #[test]
    fn the_run_config_names_its_own_project() {
        let store = tmp_store("config");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        let config = store
            .provision_run_dir("a")
            .unwrap()
            .join(".opencode/opencode.json");

        assert!(!config.symlink_metadata().unwrap().file_type().is_symlink());
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(config).unwrap()).unwrap();
        let env = &doc["mcp"]["corpus"]["environment"];
        assert_eq!(env[PROJECT_ENV].as_str(), Some("a"));
        assert_eq!(
            env[STORE_ENV].as_str(),
            Some(store.root().to_string_lossy().as_ref())
        );
        assert!(env.get(AGENT_ENV).is_none());
        assert!(env.get(RUN_LOG_ENV).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn provisioning_refuses_parent_symlinks_and_extra_projects() {
        let store = tmp_store("planted-boundary");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        store.create_project("b", "B", "cdk-regtest").unwrap();

        let run = store.provision_run_dir("a").unwrap();
        fs::remove_dir_all(run.join("store")).unwrap();
        let outside = store.root().join("outside-workspace");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, run.join("store")).unwrap();

        let error = store.provision_run_dir("a").unwrap_err();
        assert!(
            error.to_string().contains("not a real directory"),
            "{error}"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

        fs::remove_file(run.join("store")).unwrap();
        let projects = run.join("store/projects");
        fs::create_dir_all(&projects).unwrap();
        std::os::unix::fs::symlink(store.project_dir("b"), projects.join("b")).unwrap();

        let error = store.provision_run_dir("a").unwrap_err();
        assert!(
            error.to_string().contains("unexpected project entry"),
            "{error}"
        );
        assert!(!projects.join("a").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_run_source_view_exposes_only_exact_pins_and_refuses_nested_symlinks() {
        let store = tmp_store("source-view");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        let cache = store.source_cache_dir();
        let selected_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let other_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        fs::create_dir_all(cache.join("selected").join(selected_sha)).unwrap();
        fs::write(
            cache.join("selected").join(selected_sha).join("lib.rs"),
            "selected\n",
        )
        .unwrap();
        fs::create_dir_all(cache.join("other").join(other_sha)).unwrap();
        fs::write(
            cache.join("other").join(other_sha).join("secret.rs"),
            "other\n",
        )
        .unwrap();

        let pins = format!(r#"{{"selected":"{selected_sha}"}}"#);
        let run = store
            .provision_run_dir_with_sources("a", Some(&pins))
            .unwrap();
        let sources = run.join("sources");
        assert!(sources.symlink_metadata().unwrap().is_dir());
        assert!(!sources.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            fs::read_link(sources.join("selected").join(selected_sha)).unwrap(),
            cache
                .join("selected")
                .join(selected_sha)
                .canonicalize()
                .unwrap()
        );
        assert!(!sources.join("other").exists());

        let outside = store.root().join("outside-source");
        fs::write(&outside, "secret\n").unwrap();
        let linked_sha = "cccccccccccccccccccccccccccccccccccccccc";
        let linked = cache.join("linked").join(linked_sha);
        fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&outside, linked.join("leak.rs")).unwrap();
        let linked_pins = format!(r#"{{"linked":"{linked_sha}"}}"#);
        let error = store
            .provision_run_dir_with_sources("a", Some(&linked_pins))
            .unwrap_err()
            .to_string();
        assert!(error.contains("symlink or special file"), "{error}");
    }
}
