//! Atomic local installation and selection of immutable plugin bundles.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, PluginManifest, PluginManifestVersion, Result};

static INSTALL_NONCE: AtomicU64 = AtomicU64::new(1);
static VERIFIED_INSTALLS: OnceLock<Mutex<std::collections::HashMap<PathBuf, String>>> =
    OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReceipt {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub path: PathBuf,
    pub previous: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallRecord {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub installed_at: u64,
}

pub fn install_plugin_bundle(bundle: &Path) -> Result<InstallReceipt> {
    let manifest = PluginManifest::load(bundle)?;
    if manifest.manifest_version != PluginManifestVersion::V1 {
        return Err(Error::Store(
            "only manifest_version = 1 bundles can be installed".to_string(),
        ));
    }
    validate_component("plugin id", &manifest.name)?;
    let version = manifest
        .version
        .clone()
        .ok_or_else(|| Error::Store("installable plugins require a version".to_string()))?;
    validate_component("plugin version", &version)?;
    validate_exec(bundle, &manifest.exec)?;

    let digest = bundle_digest(bundle)?;
    let id_root = crate::paths::plugin_install_root().join(&manifest.name);
    let versions = id_root.join("versions");
    let destination = versions.join(&version);
    fs::create_dir_all(&versions)?;

    if destination.exists() {
        let installed = bundle_digest(&destination)?;
        if installed != digest {
            return Err(Error::Store(format!(
                "plugin {} version {} is already installed with digest {}; immutable versions cannot be overwritten by {}",
                manifest.name, version, installed, digest
            )));
        }
    } else {
        let nonce = INSTALL_NONCE.fetch_add(1, Ordering::Relaxed);
        let staging = versions.join(format!(".{version}.staging-{}-{nonce}", std::process::id()));
        fs::create_dir(&staging)?;
        if let Err(error) = copy_bundle(bundle, bundle, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        fs::rename(&staging, &destination)?;
        make_directories_read_only(&destination)?;
    }

    ensure_install_record(&id_root, &manifest.name, &version, &digest)?;
    let previous = selected_version(&manifest.name)?;
    select_plugin_version(&manifest.name, &version)?;
    Ok(InstallReceipt {
        id: manifest.name,
        version,
        digest,
        path: destination,
        previous,
    })
}

pub fn installed_record(id: &str, version: &str) -> Result<InstallRecord> {
    validate_component("plugin id", id)?;
    validate_component("plugin version", version)?;
    let path = crate::paths::plugin_install_root()
        .join(id)
        .join("metadata")
        .join(format!("{version}.json"));
    let raw = fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|error| {
        Error::Store(format!(
            "invalid install record {}: {error}",
            path.display()
        ))
    })
}

/// Verify the selected bundle against its immutable installation receipt.
/// Development overrides have no receipt, so their current digest is returned
/// directly. Call this immediately before executing plugin code.
pub fn verify_plugin_installation(plugin: &crate::PluginDir) -> Result<String> {
    if plugin.origin == crate::PluginOrigin::Installed {
        let version = plugin.manifest.version.as_deref().ok_or_else(|| {
            Error::Store(format!(
                "installed plugin {} has no version",
                plugin.manifest.name
            ))
        })?;
        let expected = installed_record(&plugin.manifest.name, version)?.digest;
        let verified =
            VERIFIED_INSTALLS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        if verified
            .lock()
            .map_err(|_| Error::Store("plugin verification cache is poisoned".into()))?
            .get(&plugin.dir)
            == Some(&expected)
        {
            return Ok(expected);
        }
        let digest = plugin_bundle_digest(&plugin.dir)?;
        if digest != expected {
            return Err(Error::Store(format!(
                "installed plugin {}@{} is damaged: bundle digest {} does not match installation receipt {}",
                plugin.manifest.name, version, digest, expected
            )));
        }
        verified
            .lock()
            .map_err(|_| Error::Store("plugin verification cache is poisoned".into()))?
            .insert(plugin.dir.clone(), expected.clone());
        return Ok(expected);
    }
    plugin_bundle_digest(&plugin.dir)
}

pub fn select_plugin_version(id: &str, version: &str) -> Result<()> {
    validate_component("plugin id", id)?;
    validate_component("plugin version", version)?;
    let id_root = crate::paths::plugin_install_root().join(id);
    let destination = id_root.join("versions").join(version);
    let manifest = PluginManifest::load(&destination)?;
    if manifest.name != id || manifest.version.as_deref() != Some(version) {
        return Err(Error::Store(format!(
            "installed manifest at {} does not identify {id}@{version}",
            destination.display()
        )));
    }
    fs::create_dir_all(&id_root)?;
    let selection = id_root.join("selected");
    let temporary = id_root.join(format!(".selected-{}", std::process::id()));
    fs::write(&temporary, format!("{version}\n"))?;
    fs::rename(&temporary, selection)?;
    Ok(())
}

pub fn selected_version(id: &str) -> Result<Option<String>> {
    validate_component("plugin id", id)?;
    match fs::read_to_string(
        crate::paths::plugin_install_root()
            .join(id)
            .join("selected"),
    ) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn plugin_lifecycle_params(plugin: &crate::PluginDir) -> Result<serde_json::Value> {
    let state = crate::paths::plugin_runtime_root().join(&plugin.manifest.name);
    let sources = crate::paths::sources_dir()?;
    fs::create_dir_all(&state)?;
    fs::create_dir_all(&sources)?;
    Ok(serde_json::json!({
        "plugin_dir": plugin.dir,
        "state_dir": state,
        "source_cache": sources,
    }))
}

/// Run one negotiated v1 lifecycle operation with corpus-owned paths. Setup
/// consults the plugin's idempotency record before retrying, so a lost terminal
/// reply cannot rebuild or mutate the environment twice.
pub fn call_plugin_lifecycle_cancellable<C, F>(
    plugin_dir: &crate::PluginDir,
    method: &str,
    deadline: std::time::Duration,
    is_cancelled: C,
    on_progress: F,
) -> Result<serde_json::Value>
where
    C: FnMut() -> bool,
    F: FnMut(&crate::LifecycleProgress),
{
    if plugin_dir.manifest.manifest_version != PluginManifestVersion::V1 {
        return Err(Error::Store(format!(
            "plugin lifecycle requires manifest v1: {}",
            plugin_dir.manifest.name
        )));
    }
    let mut params = plugin_lifecycle_params(plugin_dir)?;
    if method == "setup" {
        let source_cache = crate::paths::sources_dir()?;
        let mut sources = Vec::new();
        for source in &plugin_dir.manifest.sources {
            crate::srcrev::ensure_source_tree(
                &source_cache,
                &source.id,
                &source.repo,
                &source.default_rev,
                &source.default_sha,
            )?;
            sources.push(serde_json::json!({
                "id": source.id,
                "sha": source.default_sha,
                "host_path": source_cache.join(&source.id).join(&source.default_sha),
                "mount": source.mount,
            }));
        }
        params["sources"] = serde_json::Value::Array(sources);
    }
    let mut plugin = crate::Plugin::spawn(&plugin_dir.dir)?;
    plugin.hello()?;
    if method == "setup" {
        let key = format!(
            "setup:{}:{}",
            plugin_dir.manifest.name,
            plugin_dir.manifest.version.as_deref().unwrap_or("unknown")
        );
        params["idempotency_key"] = serde_json::Value::String(key.clone());
        let context = params.as_object().cloned().unwrap_or_default();
        let status = plugin.operation_status_with_params(&key, context)?;
        match status.state {
            crate::OperationState::Succeeded => {
                return Ok(status.result.unwrap_or(serde_json::Value::Null));
            }
            crate::OperationState::Running => {
                return Err(Error::Plugin {
                    plugin: plugin_dir.manifest.name.clone(),
                    message: format!("setup operation {key} is already running"),
                });
            }
            crate::OperationState::Failed
                if !status.error.as_ref().is_some_and(|error| error.retryable) =>
            {
                return Err(Error::Plugin {
                    plugin: plugin_dir.manifest.name.clone(),
                    message: status
                        .error
                        .map(|error| format!("{}: {}", error.code, error.message))
                        .unwrap_or_else(|| format!("setup operation {key} previously failed")),
                });
            }
            crate::OperationState::Unknown | crate::OperationState::Failed => {}
        }
    }
    plugin.lifecycle_call_cancellable(method, Some(params), deadline, is_cancelled, on_progress)
}

fn validate_component(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    {
        return Err(Error::Store(format!("invalid {label} {value:?}")));
    }
    Ok(())
}

fn ensure_install_record(id_root: &Path, id: &str, version: &str, digest: &str) -> Result<()> {
    let metadata = id_root.join("metadata");
    fs::create_dir_all(&metadata)?;
    let destination = metadata.join(format!("{version}.json"));
    if destination.exists() {
        let record: InstallRecord = serde_json::from_str(&fs::read_to_string(&destination)?)
            .map_err(|error| {
                Error::Store(format!(
                    "invalid install record {}: {error}",
                    destination.display()
                ))
            })?;
        if record.id != id || record.version != version || record.digest != digest {
            return Err(Error::Store(format!(
                "install record for {id}@{version} disagrees with immutable bundle digest"
            )));
        }
        return Ok(());
    }
    let record = InstallRecord {
        id: id.to_string(),
        version: version.to_string(),
        digest: digest.to_string(),
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let nonce = INSTALL_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = metadata.join(format!(".{version}-{}-{nonce}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(&record)?)?;
    fs::rename(temporary, destination)?;
    Ok(())
}

fn validate_exec(bundle: &Path, exec: &str) -> Result<()> {
    let relative = Path::new(exec);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(Error::Store(format!(
            "plugin exec must stay inside its bundle: {exec:?}"
        )));
    }
    let executable = bundle.join(relative);
    if !executable.is_file() {
        return Err(Error::Store(format!(
            "plugin exec is not a file: {}",
            executable.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&executable)?.permissions().mode() & 0o111 == 0 {
            return Err(Error::Store(format!(
                "plugin exec is not executable: {}",
                executable.display()
            )));
        }
    }
    Ok(())
}

fn copy_bundle(source_root: &Path, current: &Path, destination_root: &Path) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.path();
        if source
            .strip_prefix(source_root)
            .ok()
            .and_then(|path| path.components().next())
            .is_some_and(|component| component.as_os_str() == ".git")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err(Error::Store(format!(
                "plugin bundles may contain only directories and regular files: {}",
                source.display()
            )));
        }
        let relative = source
            .strip_prefix(source_root)
            .map_err(|error| Error::Store(error.to_string()))?;
        let destination = destination_root.join(relative);
        if metadata.is_dir() {
            fs::create_dir(&destination)?;
            copy_bundle(source_root, &source, destination_root)?;
        } else {
            fs::copy(&source, &destination)?;
            let mut permissions = metadata.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() & !0o222);
            }
            fs::set_permissions(destination, permissions)?;
        }
    }
    Ok(())
}

fn bundle_digest(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut digest = Sha256::new();
    for relative in files {
        let encoded = relative.to_string_lossy();
        digest.update((encoded.len() as u64).to_le_bytes());
        digest.update(encoded.as_bytes());
        let mut file = fs::File::open(root.join(&relative))?;
        let length = file.metadata()?.len();
        digest.update(length.to_le_bytes());
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn plugin_bundle_digest(root: &Path) -> Result<String> {
    bundle_digest(root)
}

fn make_directories_read_only(root: &Path) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            make_directories_read_only(&path)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in directories.drain(..) {
            let mut permissions = fs::metadata(&directory)?.permissions();
            permissions.set_mode(permissions.mode() & !0o222);
            fs::set_permissions(directory, permissions)?;
        }
    }
    Ok(())
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| component.as_os_str() == ".git")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err(Error::Store(format!(
                "unsupported bundle entry: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else {
            files.push(
                path.strip_prefix(root)
                    .map_err(|error| Error::Store(error.to_string()))?
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, unique_temp_path, EnvVarGuard};

    fn bundle(root: &Path, version: &str, body: &str) -> PathBuf {
        let dir = root.join(format!("bundle-{version}"));
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(
            dir.join("plugin.toml"),
            format!(
                "manifest_version = 1\nid = \"fixture-regtest\"\nversion = \"{version}\"\nprotocol = \"corpus.environment/1\"\nexec = \"bin/plugin\"\n"
            ),
        )
        .unwrap();
        fs::write(dir.join("bin/plugin"), body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join("bin/plugin"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[test]
    fn installs_immutable_versions_and_explicitly_rolls_back_selection() {
        let _lock = env_lock();
        let root = unique_temp_path("corpus-plugin-install");
        let _ = fs::remove_dir_all(&root);
        let _home = EnvVarGuard::set("CORPUS_HOME", root.join("home"));
        let _override = EnvVarGuard::set("CORPUS_PLUGINS_DIR", "");

        let v1 = bundle(&root, "1.0.0", "#!/bin/sh\nexit 0\n");
        fs::create_dir_all(v1.join(".git/objects")).unwrap();
        fs::write(v1.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let first = install_plugin_bundle(&v1).unwrap();
        assert_eq!(first.previous, None);
        assert_eq!(
            selected_version("fixture-regtest").unwrap().as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            installed_record("fixture-regtest", "1.0.0").unwrap().digest,
            first.digest
        );
        assert_eq!(
            crate::plugin_catalog().unwrap()[0].origin,
            crate::PluginOrigin::Installed
        );
        assert!(!first.path.join(".git").exists());

        let v2 = bundle(&root, "2.0.0", "#!/bin/sh\nexit 2\n");
        let second = install_plugin_bundle(&v2).unwrap();
        assert_eq!(second.previous.as_deref(), Some("1.0.0"));
        assert_eq!(
            crate::plugin_catalog().unwrap()[0]
                .manifest
                .version
                .as_deref(),
            Some("2.0.0")
        );
        select_plugin_version("fixture-regtest", "1.0.0").unwrap();
        assert_eq!(
            selected_version("fixture-regtest").unwrap().as_deref(),
            Some("1.0.0")
        );

        // Reinstalling identical bytes is idempotent; changing the bytes of
        // an already-installed version cannot replace its evidence identity.
        fs::write(v1.join(".git/HEAD"), "changed checkout metadata\n").unwrap();
        install_plugin_bundle(&v1).unwrap();
        fs::write(v1.join("bin/plugin"), "#!/bin/sh\nexit 9\n").unwrap();
        assert!(install_plugin_bundle(&v1)
            .unwrap_err()
            .to_string()
            .contains("immutable versions cannot be overwritten"));

        // A privileged/local mutation of the read-only selected bundle is
        // detected before execution and projected as an unhealthy status.
        let installed_exec = first.path.join("bin/plugin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&installed_exec, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(&installed_exec, "#!/bin/sh\nexit 7\n").unwrap();
        let error = crate::find_plugin("fixture-regtest")
            .unwrap_err()
            .to_string();
        assert!(error.contains("is damaged"), "{error}");
        let status = crate::selected_plugin_status(Some("fixture-regtest"));
        let selected = status
            .iter()
            .find(|candidate| candidate.name == "fixture-regtest")
            .unwrap();
        assert!(!selected.ready);
        assert!(selected.notes.contains("bundle verification failed"));

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_and_non_executable_entrypoints() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let _lock = env_lock();
        let root = unique_temp_path("corpus-plugin-install-invalid");
        let _ = fs::remove_dir_all(&root);
        let _home = EnvVarGuard::set("CORPUS_HOME", root.join("home"));
        let _override = EnvVarGuard::set("CORPUS_PLUGINS_DIR", "");

        let bad_exec = bundle(&root, "1.0.0", "#!/bin/sh\n");
        fs::set_permissions(
            bad_exec.join("bin/plugin"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(install_plugin_bundle(&bad_exec)
            .unwrap_err()
            .to_string()
            .contains("not executable"));

        let linked = bundle(&root, "2.0.0", "#!/bin/sh\n");
        symlink("plugin.toml", linked.join("manifest-link")).unwrap();
        assert!(install_plugin_bundle(&linked)
            .unwrap_err()
            .to_string()
            .contains("unsupported bundle entry"));
        let _ = fs::remove_dir_all(&root);
    }
}
