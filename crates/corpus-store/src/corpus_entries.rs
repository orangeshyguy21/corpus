//! Confined corpus-entry path resolution and mutation.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::filesystem::atomic_write;
use crate::store::{Store, CATEGORIES, LEGACY_ATTACKS, RUNS};

/// What a caller intends to do with a corpus entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAccess {
    /// Read an existing entry, including immutable run transcripts.
    Read,
    /// Change or remove an existing entry.
    Mutate,
    /// Name a destination that need not exist yet.
    Destination,
}

impl EntryAccess {
    fn is_mutation(self) -> bool {
        matches!(self, Self::Mutate | Self::Destination)
    }
}

impl Store {
    /// Resolve a caller-supplied relative path inside one project's corpus.
    ///
    /// Textual component checks reject traversal and re-rooting. Canonical
    /// containment then rejects symlinks planted inside the agent-writable
    /// corpus. Mutations additionally refuse run transcripts and bare
    /// categories.
    pub fn resolve_corpus_entry(
        &self,
        project: &str,
        rel: &str,
        access: EntryAccess,
    ) -> Result<PathBuf> {
        let rel = rel.trim();
        if rel.is_empty() {
            return Err(Error::Store("path is empty".into()));
        }
        let path = Path::new(rel);
        if path.is_absolute() {
            return Err(Error::Store(format!(
                "path must be relative to the project corpus: {rel}"
            )));
        }
        let mut components = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(part) => components.push(part),
                other => {
                    return Err(Error::Store(format!(
                        "path component {other:?} is not allowed inside a corpus: {rel}"
                    )))
                }
            }
        }
        let category = components
            .first()
            .and_then(|component| component.to_str())
            .ok_or_else(|| Error::Store(format!("path names no category: {rel}")))?;
        let legacy_read_or_delete =
            category == LEGACY_ATTACKS && access != EntryAccess::Destination;
        if !CATEGORIES.contains(&category) && !legacy_read_or_delete {
            return Err(Error::Store(format!(
                "{category:?} is not a corpus category (one of {})",
                CATEGORIES.join(", ")
            )));
        }
        if category == RUNS && access.is_mutation() {
            return Err(Error::Store(format!(
                "{RUNS}/ holds the mission transcripts: technique cards cite them by name, the \
                 cost report counts them, and they are what an operator reads to audit a run. \
                 They can be read, never changed: {rel}"
            )));
        }
        if components.len() == 1 && access.is_mutation() {
            return Err(Error::Store(format!(
                "{rel} is a whole category, not an entry — removing one wholesale is a corpus \
                 wipe under another name"
            )));
        }

        let root = self
            .project_corpus_dir(project)
            .canonicalize()
            .map_err(|error| {
                Error::Store(format!(
                    "project {project} has no corpus directory ({error}) — create the project first"
                ))
            })?;
        let joined = root.join(path);
        let resolved = match access {
            EntryAccess::Read | EntryAccess::Mutate => joined
                .canonicalize()
                .map_err(|error| Error::Store(format!("{rel}: {error}")))?,
            EntryAccess::Destination => resolve_destination(&joined, rel)?,
        };
        if !resolved.starts_with(&root) {
            return Err(Error::Store(format!(
                "{rel} resolves outside the project corpus — a link inside a corpus does not \
                 widen it"
            )));
        }
        Ok(resolved)
    }

    /// Delete one confined entry and return the bytes freed.
    pub fn delete_corpus_entry(&self, project: &str, rel: &str, recursive: bool) -> Result<u64> {
        let path = self.resolve_corpus_entry(project, rel, EntryAccess::Mutate)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            if !recursive {
                return Err(Error::Store(format!(
                    "{rel} is a directory — pass recursive to remove it and everything under it"
                )));
            }
            let bytes = dir_bytes(&path);
            fs::remove_dir_all(&path)?;
            return Ok(bytes);
        }
        let bytes = metadata.len();
        fs::remove_file(&path)?;
        Ok(bytes)
    }

    /// Move one entry within the same confined corpus.
    pub fn move_corpus_entry(
        &self,
        project: &str,
        from: &str,
        to: &str,
        overwrite: bool,
    ) -> Result<()> {
        let source = self.resolve_corpus_entry(project, from, EntryAccess::Mutate)?;
        let destination = self.resolve_corpus_entry(project, to, EntryAccess::Destination)?;
        if source == destination {
            return Ok(());
        }
        if destination.symlink_metadata().is_ok() {
            if !overwrite {
                return Err(Error::Store(format!(
                    "{to} already exists — pass overwrite to replace it"
                )));
            }
            match destination.is_dir() {
                true => fs::remove_dir_all(&destination)?,
                false => fs::remove_file(&destination)?,
            }
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(source, destination)?;
        Ok(())
    }

    /// Atomically create or replace one confined file entry.
    pub fn write_corpus_entry(&self, project: &str, rel: &str, content: &str) -> Result<u64> {
        let path = self.resolve_corpus_entry(project, rel, EntryAccess::Destination)?;
        if path
            .symlink_metadata()
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            return Err(Error::Store(format!(
                "{rel} is a directory — entry_write replaces a file, not a tree"
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&path, content)?;
        Ok(content.len() as u64)
    }
}

/// Canonicalize the deepest existing ancestor and append the validated tail.
fn resolve_destination(joined: &Path, rel: &str) -> Result<PathBuf> {
    let mut existing = joined.to_path_buf();
    let mut tail = Vec::new();
    while !existing.exists() {
        tail.push(
            existing
                .file_name()
                .ok_or_else(|| Error::Store(format!("{rel} names no file")))?
                .to_os_string(),
        );
        existing = existing
            .parent()
            .ok_or_else(|| Error::Store(format!("{rel} has no parent")))?
            .to_path_buf();
    }
    let mut resolved = existing
        .canonicalize()
        .map_err(|error| Error::Store(format!("{rel}: {error}")))?;
    for part in tail.iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

/// Total bytes below a directory, best-effort before recursive deletion.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                dir_bytes(&path)
            } else {
                path.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            }
        })
        .sum()
}
