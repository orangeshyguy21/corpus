//! Corpus-authored distribution metadata for supported environment plugins.
//!
//! The plugin manifest remains authoritative for runtime behavior. This
//! catalog owns only the public artifact Corpus is willing to download and
//! install for an operator.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{Error, InstallReceipt, PluginManifest, Result};

const BUILT_IN_CATALOG: &str = include_str!("../../../plugin-catalog.toml");
const SUPPORTED_CATALOG_VERSION: u32 = 1;
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 4096;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CuratedPlugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub requirements: Vec<CuratedPluginRequirement>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CuratedPluginRequirement {
    Docker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CuratedInstallPhase {
    Downloading,
    Verifying,
    Extracting,
    Installing,
}

impl CuratedInstallPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Downloading => "downloading release",
            Self::Verifying => "verifying checksum",
            Self::Extracting => "extracting bundle",
            Self::Installing => "installing immutable version",
        }
    }
}

#[derive(Debug, Deserialize)]
struct PluginCatalog {
    catalog_version: u32,
    #[serde(default)]
    plugins: Vec<CuratedPlugin>,
}

pub fn curated_plugins() -> Result<Vec<CuratedPlugin>> {
    parse_catalog(BUILT_IN_CATALOG)
}

pub fn curated_plugin(id: &str) -> Result<CuratedPlugin> {
    curated_plugins()?
        .into_iter()
        .find(|plugin| plugin.id == id)
        .ok_or_else(|| Error::Store(format!("unsupported curated plugin: {id}")))
}

pub fn install_curated_plugin(id: &str) -> Result<InstallReceipt> {
    install_curated_plugin_with(id, || false, |_| {})
}

pub fn install_curated_plugin_with<C, F>(
    id: &str,
    mut is_cancelled: C,
    mut on_phase: F,
) -> Result<InstallReceipt>
where
    C: FnMut() -> bool,
    F: FnMut(CuratedInstallPhase),
{
    let plugin = curated_plugin(id)?;
    let staging = tempfile::Builder::new()
        .prefix("corpus-plugin-")
        .tempdir()
        .map_err(|error| {
            Error::Store(format!("cannot create plugin staging directory: {error}"))
        })?;
    let archive_path = staging.path().join("bundle.tar.gz");

    on_phase(CuratedInstallPhase::Downloading);
    download_archive(&plugin, &archive_path, &mut is_cancelled)?;
    if is_cancelled() {
        return Err(Error::Store("plugin installation cancelled".into()));
    }

    on_phase(CuratedInstallPhase::Verifying);
    verify_archive_checksum(&archive_path, &plugin.sha256)?;

    on_phase(CuratedInstallPhase::Extracting);
    let extracted = staging.path().join("extracted");
    fs::create_dir(&extracted)?;
    extract_archive(&archive_path, &extracted, &mut is_cancelled)?;
    let bundle = extracted_bundle_root(&extracted)?;
    verify_catalog_identity(&plugin, &bundle)?;

    if is_cancelled() {
        return Err(Error::Store("plugin installation cancelled".into()));
    }
    on_phase(CuratedInstallPhase::Installing);
    crate::install_plugin_bundle(&bundle)
}

fn download_archive<C>(
    plugin: &CuratedPlugin,
    destination: &Path,
    is_cancelled: &mut C,
) -> Result<()>
where
    C: FnMut() -> bool,
{
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("corpus/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| Error::Store(format!("cannot create plugin download client: {error}")))?;
    let mut response = client
        .get(&plugin.url)
        .send()
        .map_err(|error| Error::Store(format!("could not download {}: {error}", plugin.name)))?
        .error_for_status()
        .map_err(|error| {
            Error::Store(format!(
                "plugin release for {} returned an error: {error}",
                plugin.name
            ))
        })?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err(Error::Store(format!(
            "plugin release for {} exceeds the {} MiB download limit",
            plugin.name,
            MAX_ARCHIVE_BYTES / 1024 / 1024
        )));
    }

    let mut output = File::create(destination)?;
    let mut buffer = [0_u8; 32 * 1024];
    let mut total = 0_u64;
    loop {
        if is_cancelled() {
            return Err(Error::Store("plugin installation cancelled".into()));
        }
        let read = response.read(&mut buffer).map_err(|error| {
            Error::Store(format!(
                "plugin download for {} failed: {error}",
                plugin.name
            ))
        })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > MAX_ARCHIVE_BYTES {
            return Err(Error::Store(format!(
                "plugin release for {} exceeds the {} MiB download limit",
                plugin.name,
                MAX_ARCHIVE_BYTES / 1024 / 1024
            )));
        }
        output.write_all(&buffer[..read])?;
    }
    output.sync_all()?;
    Ok(())
}

fn verify_archive_checksum(path: &Path, expected: &str) -> Result<()> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(Error::Store(format!(
            "plugin archive checksum mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

fn extract_archive<C>(archive_path: &Path, destination: &Path, is_cancelled: &mut C) -> Result<()>
where
    C: FnMut() -> bool,
{
    let archive = File::open(archive_path)?;
    let decoder = GzDecoder::new(archive);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = 0_usize;
    let mut expanded = 0_u64;

    for entry in archive
        .entries()
        .map_err(|error| Error::Store(format!("cannot read plugin archive: {error}")))?
    {
        if is_cancelled() {
            return Err(Error::Store("plugin installation cancelled".into()));
        }
        entries += 1;
        if entries > MAX_ARCHIVE_ENTRIES {
            return Err(Error::Store(format!(
                "plugin archive exceeds the {MAX_ARCHIVE_ENTRIES} entry limit"
            )));
        }
        let mut entry = entry
            .map_err(|error| Error::Store(format!("cannot read plugin archive entry: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| Error::Store(format!("invalid plugin archive path: {error}")))?
            .into_owned();
        validate_archive_path(&path)?;
        let target = destination.join(&path);
        let kind = entry.header().entry_type();
        let size = entry
            .header()
            .size()
            .map_err(|error| Error::Store(format!("invalid archive entry size: {error}")))?;
        expanded = expanded.saturating_add(size);
        if expanded > MAX_EXPANDED_BYTES {
            return Err(Error::Store(format!(
                "plugin archive exceeds the {} MiB expanded-size limit",
                MAX_EXPANDED_BYTES / 1024 / 1024
            )));
        }
        // BSD tar emits this metadata entry in GitHub release archives. The
        // tar crate applies its attributes to following entries; it must not
        // be materialized as a plugin file.
        if kind.is_pax_global_extensions() {
            continue;
        }
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if !kind.is_file() {
            return Err(Error::Store(format!(
                "plugin archive contains unsupported entry {}",
                path.display()
            )));
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(&target)?;
        let copied = std::io::copy(&mut entry, &mut output)?;
        if copied != size {
            return Err(Error::Store(format!(
                "plugin archive entry {} was truncated",
                path.display()
            )));
        }
        set_archive_mode(&target, entry.header().mode().unwrap_or(0o644))?;
    }
    Ok(())
}

fn validate_archive_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::Store(format!(
            "plugin archive path escapes its staging directory: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn set_archive_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_archive_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn extracted_bundle_root(extracted: &Path) -> Result<PathBuf> {
    if extracted.join("plugin.toml").is_file() {
        return Ok(extracted.to_path_buf());
    }
    let children = fs::read_dir(extracted)?.collect::<std::result::Result<Vec<_>, _>>()?;
    if children.len() == 1 && children[0].file_type()?.is_dir() {
        let root = children[0].path();
        if root.join("plugin.toml").is_file() {
            return Ok(root);
        }
    }
    Err(Error::Store(
        "plugin archive must contain plugin.toml at its root or in one top-level directory".into(),
    ))
}

fn verify_catalog_identity(plugin: &CuratedPlugin, bundle: &Path) -> Result<()> {
    let manifest = PluginManifest::load(bundle)?;
    if manifest.name != plugin.id || manifest.version.as_deref() != Some(&plugin.version) {
        return Err(Error::Store(format!(
            "plugin archive identifies {}@{}, expected {}@{}",
            manifest.name,
            manifest.version.as_deref().unwrap_or("unversioned"),
            plugin.id,
            plugin.version
        )));
    }
    Ok(())
}

fn parse_catalog(raw: &str) -> Result<Vec<CuratedPlugin>> {
    let catalog: PluginCatalog = toml::from_str(raw)
        .map_err(|error| Error::Store(format!("invalid built-in plugin catalog: {error}")))?;
    if catalog.catalog_version != SUPPORTED_CATALOG_VERSION {
        return Err(Error::Store(format!(
            "unsupported built-in plugin catalog version {}",
            catalog.catalog_version
        )));
    }

    let mut ids = BTreeSet::new();
    for plugin in &catalog.plugins {
        validate_component("plugin id", &plugin.id)?;
        validate_component("plugin version", &plugin.version)?;
        if plugin.name.trim().is_empty() || plugin.description.trim().is_empty() {
            return Err(Error::Store(format!(
                "curated plugin {} requires a name and description",
                plugin.id
            )));
        }
        if !plugin.url.starts_with("https://") {
            return Err(Error::Store(format!(
                "curated plugin {} requires an HTTPS artifact URL",
                plugin.id
            )));
        }
        if plugin.sha256.len() != 64
            || !plugin
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::Store(format!(
                "curated plugin {} has an invalid SHA-256 digest",
                plugin.id
            )));
        }
        if !ids.insert(plugin.id.clone()) {
            return Err(Error::Store(format!(
                "duplicate curated plugin id: {}",
                plugin.id
            )));
        }
    }
    Ok(catalog.plugins)
}

fn validate_component(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(Error::Store(format!("invalid {label}: {value:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    fn append_file(
        builder: &mut tar::Builder<GzEncoder<Vec<u8>>>,
        path: &str,
        body: &[u8],
        mode: u32,
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder.append_data(&mut header, path, body).unwrap();
    }

    fn fixture_archive(id: &str, version: &str) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let pax = b"27 comment=fixture archive\n";
        let mut pax_header = tar::Header::new_gnu();
        pax_header.set_entry_type(tar::EntryType::XGlobalHeader);
        pax_header.set_size(pax.len() as u64);
        pax_header.set_mode(0o644);
        pax_header.set_cksum();
        builder
            .append_data(&mut pax_header, "pax_global_header", pax.as_slice())
            .unwrap();
        let root = format!("fixture-{version}");
        append_file(
            &mut builder,
            &format!("{root}/plugin.toml"),
            format!(
                "manifest_version = 1\nid = \"{id}\"\nversion = \"{version}\"\nprotocol = \"corpus.environment/1\"\nexec = \"plugin\"\n"
            )
            .as_bytes(),
            0o644,
        );
        append_file(
            &mut builder,
            &format!("{root}/plugin"),
            b"#!/bin/sh\nexit 0\n",
            0o755,
        );
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn fixture_plugin(id: &str, version: &str, bytes: &[u8]) -> CuratedPlugin {
        CuratedPlugin {
            id: id.into(),
            name: "Fixture".into(),
            description: "Fixture plugin".into(),
            version: version.into(),
            url: "https://example.invalid/plugin.tar.gz".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            requirements: vec![CuratedPluginRequirement::Docker],
        }
    }

    #[test]
    fn built_in_catalog_is_valid_and_stably_ordered() {
        let plugins = curated_plugins().unwrap();
        assert_eq!(
            plugins
                .iter()
                .map(|plugin| plugin.id.as_str())
                .collect::<Vec<_>>(),
            ["cdk-regtest", "nutshell-regtest"]
        );
        assert!(plugins
            .iter()
            .all(|plugin| plugin.requirements == [CuratedPluginRequirement::Docker]));
    }

    #[test]
    fn catalog_rejects_duplicate_ids_and_untrusted_metadata() {
        let duplicate = BUILT_IN_CATALOG.replace("nutshell-regtest", "cdk-regtest");
        assert!(parse_catalog(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate curated plugin id"));

        let insecure = BUILT_IN_CATALOG.replacen("https://", "http://", 1);
        assert!(parse_catalog(&insecure)
            .unwrap_err()
            .to_string()
            .contains("HTTPS artifact URL"));

        let marker = "sha256 = \"";
        let digest_start = BUILT_IN_CATALOG.find(marker).unwrap() + marker.len();
        let digest_end = digest_start + BUILT_IN_CATALOG[digest_start..].find('"').unwrap();
        let mut bad_digest = BUILT_IN_CATALOG.to_string();
        bad_digest.replace_range(digest_start..digest_end, "not-a-digest");
        assert!(parse_catalog(&bad_digest)
            .unwrap_err()
            .to_string()
            .contains("invalid SHA-256"));
    }

    #[test]
    fn verified_archive_extracts_one_matching_bundle() {
        let bytes = fixture_archive("fixture-regtest", "1.0.0");
        let plugin = fixture_plugin("fixture-regtest", "1.0.0", &bytes);
        let staging = tempfile::tempdir().unwrap();
        let archive = staging.path().join("fixture.tar.gz");
        fs::write(&archive, &bytes).unwrap();
        verify_archive_checksum(&archive, &plugin.sha256).unwrap();

        let extracted = staging.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        extract_archive(&archive, &extracted, &mut || false).unwrap();
        let bundle = extracted_bundle_root(&extracted).unwrap();
        verify_catalog_identity(&plugin, &bundle).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(bundle.join("plugin"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
    }

    #[test]
    fn archive_checksum_and_manifest_identity_fail_closed() {
        let bytes = fixture_archive("fixture-regtest", "1.0.0");
        let staging = tempfile::tempdir().unwrap();
        let archive = staging.path().join("fixture.tar.gz");
        fs::write(&archive, &bytes).unwrap();
        assert!(verify_archive_checksum(&archive, &"0".repeat(64))
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));

        let extracted = staging.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        extract_archive(&archive, &extracted, &mut || false).unwrap();
        let bundle = extracted_bundle_root(&extracted).unwrap();
        let wrong = fixture_plugin("another-plugin", "1.0.0", &bytes);
        assert!(verify_catalog_identity(&wrong, &bundle)
            .unwrap_err()
            .to_string()
            .contains("expected another-plugin@1.0.0"));
    }

    #[test]
    fn archive_paths_must_remain_relative_and_normal() {
        assert!(validate_archive_path(Path::new("plugin/plugin.toml")).is_ok());
        assert!(validate_archive_path(Path::new("../plugin.toml")).is_err());
        assert!(validate_archive_path(Path::new("./plugin.toml")).is_err());
        assert!(validate_archive_path(Path::new("/plugin.toml")).is_err());
    }
}
