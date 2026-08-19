//! Typed, read-only inspection of installed plugin bundles.
//!
//! Parsing a manifest is deliberately separate from executing its entrypoint.
//! Operator administration may depend on these types without gaining the
//! ability to start Docker, call an oracle, or enter a sandbox.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use corpus_store::Error;
use serde::Deserialize;

pub const ENVIRONMENT_PROTOCOL_V1: &str = "corpus.environment/1";
pub const SUPPORTED_CAPABILITIES: &[&str] = &[
    "sessions",
    "sandbox.exec",
    "faucet.bolt11",
    "wallet.fund",
    "oracle.run",
    "lifecycle.setup",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginManifestVersion {
    LegacyV0,
    V1,
}

/// A validated plugin manifest. `name` is the canonical plugin id for both
/// legacy manifests (`name`) and v1 manifests (`id`).
#[derive(Debug, Clone)]
pub struct PluginManifest {
    pub manifest_version: PluginManifestVersion,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub protocol: Option<String>,
    pub exec: String,
    pub env: HashMap<String, String>,
    pub capabilities: Vec<String>,
    pub sources: Vec<PluginSource>,
    pub environment_dependencies: Vec<EnvironmentDependency>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginSource {
    pub id: String,
    pub repo: String,
    pub default_rev: String,
    pub default_sha: String,
    pub mount: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct EnvironmentDependency {
    pub id: String,
    pub repo: String,
    pub sha: String,
    #[serde(default)]
    pub expose_to_agent: bool,
}

#[derive(Debug, Clone)]
pub struct PluginDir {
    pub dir: PathBuf,
    pub manifest: PluginManifest,
    pub origin: PluginOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOrigin {
    Direct,
    Installed,
}

#[derive(Debug, Deserialize)]
struct RawPluginManifest {
    manifest_version: Option<u32>,
    name: Option<String>,
    id: Option<String>,
    version: Option<String>,
    description: Option<String>,
    protocol: Option<String>,
    exec: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    sources: Vec<PluginSource>,
    #[serde(default)]
    environment_dependencies: Vec<EnvironmentDependency>,
}

impl PluginManifest {
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let path = dir.join("plugin.toml");
        let raw = fs::read_to_string(&path)
            .map_err(|error| Error::Manifest(path.clone(), error.to_string()))?;
        Self::parse(&path, &raw)
    }

    pub fn parse(path: &Path, raw: &str) -> Result<Self, Error> {
        let raw: RawPluginManifest = toml::from_str(raw)
            .map_err(|error| Error::Manifest(path.to_path_buf(), error.to_string()))?;
        let invalid = |message: &str| Error::Manifest(path.to_path_buf(), message.to_string());
        let exec = raw
            .exec
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid("missing non-empty exec"))?;

        let (manifest_version, name, protocol) = match raw.manifest_version {
            None => {
                if raw.id.is_some() || raw.protocol.is_some() {
                    return Err(invalid(
                        "v1 fields require manifest_version = 1; v1 is never inferred",
                    ));
                }
                let name = raw
                    .name
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| invalid("legacy manifest requires non-empty name"))?;
                (PluginManifestVersion::LegacyV0, name, None)
            }
            Some(1) => {
                let id = raw
                    .id
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| invalid("manifest v1 requires non-empty id"))?;
                let protocol = raw
                    .protocol
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| invalid("manifest v1 requires protocol"))?;
                if protocol != ENVIRONMENT_PROTOCOL_V1 {
                    return Err(invalid(&format!(
                        "unsupported plugin protocol {protocol:?}"
                    )));
                }
                if raw.name.is_some() {
                    return Err(invalid("manifest v1 uses id, not legacy name"));
                }
                (PluginManifestVersion::V1, id, Some(protocol))
            }
            Some(version) => {
                return Err(invalid(&format!(
                    "unsupported plugin manifest version {version}"
                )))
            }
        };

        if manifest_version == PluginManifestVersion::V1 {
            let mut capabilities = HashSet::new();
            for capability in &raw.capabilities {
                if capability.trim().is_empty() || !capabilities.insert(capability) {
                    return Err(invalid(
                        "manifest v1 capabilities must be non-empty and unique",
                    ));
                }
                if !SUPPORTED_CAPABILITIES.contains(&capability.as_str()) {
                    return Err(invalid(&format!(
                        "unsupported plugin capability {capability:?}"
                    )));
                }
            }
            let mut source_ids = HashSet::new();
            for source in &raw.sources {
                if source.id.trim().is_empty()
                    || source.repo.trim().is_empty()
                    || source.default_rev.trim().is_empty()
                    || !is_sha(&source.default_sha)
                    || !Path::new(&source.mount).is_absolute()
                    || !source_ids.insert(&source.id)
                {
                    return Err(invalid(
                        "manifest v1 sources require unique non-empty ids, repo/rev, a 40-hex sha, and an absolute mount",
                    ));
                }
            }
            let mut dependency_ids = HashSet::new();
            for dependency in &raw.environment_dependencies {
                if dependency.id.trim().is_empty()
                    || dependency.repo.trim().is_empty()
                    || !is_sha(&dependency.sha)
                    || dependency.expose_to_agent
                    || !dependency_ids.insert(&dependency.id)
                {
                    return Err(invalid(
                        "manifest v1 environment dependencies require unique ids, repo, a 40-hex sha, and expose_to_agent = false",
                    ));
                }
            }
        }

        Ok(Self {
            manifest_version,
            name,
            version: raw.version,
            description: raw.description,
            protocol,
            exec,
            env: raw.env,
            capabilities: raw.capabilities,
            sources: raw.sources,
            environment_dependencies: raw.environment_dependencies,
        })
    }
}

fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Discover valid plugin manifests without starting plugin processes. A bad
/// bundle is skipped so it cannot hide otherwise healthy installations.
pub fn discover_plugins(root: &Path) -> Result<Vec<PluginDir>, Error> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("plugin.toml").is_file())
        .collect();
    entries.sort();

    let mut plugins = Vec::new();
    for dir in entries {
        match PluginManifest::load(&dir) {
            Ok(manifest) => plugins.push(PluginDir {
                dir,
                manifest,
                origin: PluginOrigin::Direct,
            }),
            Err(error) => eprintln!("corpus: skipping {}: {error}", dir.display()),
        }
    }
    Ok(plugins)
}

/// The effective catalog. `CORPUS_PLUGINS_DIR` is a complete development/test
/// override. Production discovery reads only explicitly selected, immutable
/// installations under `CORPUS_HOME/plugins`.
pub fn plugin_catalog() -> Result<Vec<PluginDir>, Error> {
    if let Some(root) = std::env::var(corpus_store::paths::PLUGINS_DIR_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return discover_plugins(Path::new(&root));
    }

    let mut catalog = discover_selected_installs(&corpus_store::paths::plugin_install_root())?;
    catalog.sort_by(|left, right| left.manifest.name.cmp(&right.manifest.name));
    Ok(catalog)
}

fn discover_selected_installs(root: &Path) -> Result<Vec<PluginDir>, Error> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    ids.sort();
    let mut selected = Vec::new();
    for id_dir in ids {
        let Ok(version) = fs::read_to_string(id_dir.join("selected")) else {
            continue;
        };
        let version = version.trim();
        if version.is_empty() || version.contains('/') || version.contains("..") {
            eprintln!("corpus: ignoring invalid selection in {}", id_dir.display());
            continue;
        }
        let dir = id_dir.join("versions").join(version);
        match PluginManifest::load(&dir) {
            Ok(manifest)
                if id_dir.file_name().and_then(|name| name.to_str())
                    == Some(manifest.name.as_str())
                    && manifest.version.as_deref() == Some(version) =>
            {
                selected.push(PluginDir {
                    dir,
                    manifest,
                    origin: PluginOrigin::Installed,
                });
            }
            Ok(_) => eprintln!(
                "corpus: ignoring selected plugin whose id/version disagrees with {}",
                id_dir.display()
            ),
            Err(error) => eprintln!("corpus: ignoring {}: {error}", dir.display()),
        }
    }
    Ok(selected)
}

pub fn catalog_plugin(name: &str) -> Result<Option<PluginDir>, Error> {
    Ok(plugin_catalog()?
        .into_iter()
        .find(|plugin| plugin.manifest.name == name))
}

pub fn plugin_by_name(root: &Path, name: &str) -> Result<Option<PluginDir>, Error> {
    Ok(discover_plugins(root)?
        .into_iter()
        .find(|plugin| plugin.manifest.name == name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(raw: &str) -> Result<PluginManifest, Error> {
        PluginManifest::parse(Path::new("fixture/plugin.toml"), raw)
    }

    #[test]
    fn legacy_manifest_is_explicitly_v0() {
        let parsed = manifest(
            r#"
name = "cdk-regtest"
version = "0.1.0"
exec = "plugin"
"#,
        )
        .unwrap();
        assert_eq!(parsed.manifest_version, PluginManifestVersion::LegacyV0);
        assert_eq!(parsed.name, "cdk-regtest");
        assert_eq!(parsed.protocol, None);
    }

    #[test]
    fn v1_manifest_normalizes_id_and_typed_sources() {
        let parsed = manifest(
            r#"
manifest_version = 1
id = "nutshell-regtest"
version = "0.1.0"
protocol = "corpus.environment/1"
exec = "bin/plugin"
capabilities = ["sessions", "sandbox.exec"]

[[sources]]
id = "nutshell"
repo = "cashubtc/nutshell"
default_rev = "main"
default_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
mount = "/opt/src/nutshell"

[[environment_dependencies]]
id = "cashu-regtest"
repo = "callebtc/cashu-regtest"
sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
expose_to_agent = false
"#,
        )
        .unwrap();
        assert_eq!(parsed.manifest_version, PluginManifestVersion::V1);
        assert_eq!(parsed.name, "nutshell-regtest");
        assert_eq!(parsed.sources[0].id, "nutshell");
        assert!(!parsed.environment_dependencies[0].expose_to_agent);
    }

    #[test]
    fn v1_is_not_inferred_and_unknown_versions_fail() {
        let missing_version = manifest(
            r#"
id = "nutshell-regtest"
protocol = "corpus.environment/1"
exec = "bin/plugin"
"#,
        )
        .unwrap_err();
        assert!(missing_version.to_string().contains("never inferred"));

        let unknown = manifest(
            r#"
manifest_version = 2
id = "future"
protocol = "corpus.environment/2"
exec = "bin/plugin"
"#,
        )
        .unwrap_err();
        assert!(unknown
            .to_string()
            .contains("unsupported plugin manifest version 2"));
    }

    #[test]
    fn malformed_v1_source_is_rejected_before_execution() {
        let error = manifest(
            r#"
manifest_version = 1
id = "nutshell-regtest"
protocol = "corpus.environment/1"
exec = "bin/plugin"

[[sources]]
id = "nutshell"
repo = "cashubtc/nutshell"
default_rev = "main"
default_sha = "not-a-sha"
mount = "relative/source"
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("40-hex sha"));
    }

    #[test]
    fn unknown_v1_capability_is_rejected_before_execution() {
        let error = manifest(
            r#"
manifest_version = 1
id = "future-plugin"
protocol = "corpus.environment/1"
exec = "bin/plugin"
capabilities = ["host.shell"]
"#,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported plugin capability \"host.shell\""));
    }
}
