//! Explicit migration from the legacy `attacks/` artifact namespace.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::store::{validate_slug, Store, LEGACY_ATTACKS, PROBES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeMigration {
    pub project: String,
    pub applied: bool,
    pub actions: Vec<String>,
}

impl ProbeMigration {
    pub fn changed(&self) -> bool {
        !self.actions.is_empty()
    }
}

impl Store {
    /// Preview or apply one project's idempotent `attacks/` -> `probes/`
    /// namespace migration. The complete legacy tree is validated before the
    /// first write and a mixed non-empty layout is never merged implicitly.
    pub fn migrate_project_probes(&self, project: &str, apply: bool) -> Result<ProbeMigration> {
        validate_slug(project)?;
        if !self.project_dir(project).join("project.yaml").is_file() {
            return Err(Error::Store(format!("project not found: {project}")));
        }
        let corpus = self.project_corpus_dir(project);
        let legacy = corpus.join(LEGACY_ATTACKS);
        let current = corpus.join(PROBES);
        let legacy_state = directory_state(&legacy)?;
        let current_state = directory_state(&current)?;

        if legacy_state.non_empty && current_state.non_empty {
            return Err(Error::Store(format!(
                "project {project} has entries in both {LEGACY_ATTACKS}/ and {PROBES}/; resolve the collision before migrating"
            )));
        }

        let mut actions = Vec::new();
        let mut markdown = Vec::new();
        if legacy_state.non_empty {
            validate_legacy_entries(&legacy)?;
            markdown = legacy_attack_markdown(&legacy)?;
            for relative in &markdown {
                let destination = relative.with_file_name("probe.md");
                actions.push(format!(
                    "rename {LEGACY_ATTACKS}/{} to {LEGACY_ATTACKS}/{}",
                    relative.display(),
                    destination.display()
                ));
            }
            actions.push(format!("rename {LEGACY_ATTACKS}/ to {PROBES}/"));
        } else if legacy_state.exists {
            actions.push(format!("remove empty {LEGACY_ATTACKS}/"));
        }
        if !current_state.exists && !legacy_state.non_empty {
            actions.push(format!("create {PROBES}/"));
        }

        if apply && !actions.is_empty() {
            if legacy_state.non_empty {
                if current_state.exists {
                    fs::remove_dir(&current)?;
                }
                // Rename entry metadata first. If the process stops between
                // files, validation accepts probe.md as a completed entry
                // step and a rerun finishes the remaining files. The final
                // category rename is one same-filesystem operation.
                for relative in markdown {
                    let from = legacy.join(&relative);
                    let to = from.with_file_name("probe.md");
                    fs::rename(from, to)?;
                }
                fs::rename(&legacy, &current)?;
            } else {
                if legacy_state.exists {
                    fs::remove_dir(&legacy)?;
                }
                if !current_state.exists {
                    fs::create_dir(&current)?;
                }
            }
        }

        Ok(ProbeMigration {
            project: project.to_string(),
            applied: apply,
            actions,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct DirectoryState {
    exists: bool,
    non_empty: bool,
}

fn directory_state(path: &Path) -> Result<DirectoryState> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Ok(DirectoryState {
                exists: true,
                non_empty: fs::read_dir(path)?.next().transpose()?.is_some(),
            })
        }
        Ok(_) => Err(Error::Store(format!(
            "migration path {} is not a real directory",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DirectoryState {
            exists: false,
            non_empty: false,
        }),
        Err(error) => Err(error.into()),
    }
}

fn validate_legacy_entries(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::Store(format!(
                "legacy probe entry {} is not a real directory",
                path.display()
            )));
        }
        let run_path = path.join("run.sh");
        require_regular_file(&run_path, &path, "run.sh")?;
        let attack_path = path.join("attack.md");
        let probe_path = path.join("probe.md");
        let attack_exists = attack_path.exists();
        let probe_exists = probe_path.exists();
        if attack_exists && probe_exists {
            return Err(Error::Store(format!(
                "legacy probe entry {} contains both attack.md and probe.md",
                path.display()
            )));
        }
        if !attack_exists && !probe_exists {
            return Err(Error::Store(format!(
                "legacy probe entry {} is missing attack.md or probe.md",
                path.display()
            )));
        }
        if attack_exists {
            require_regular_file(&attack_path, &path, "attack.md")?;
        } else {
            require_regular_file(&probe_path, &path, "probe.md")?;
        }
        refuse_nested_symlinks(&path)?;
    }
    Ok(())
}

fn require_regular_file(path: &Path, entry: &Path, name: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::Store(format!(
            "legacy probe entry {} is missing {name}: {error}",
            entry.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Store(format!(
            "legacy probe file {} is not a regular file",
            path.display()
        )));
    }
    Ok(())
}

fn refuse_nested_symlinks(root: &Path) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(Error::Store(format!(
                    "legacy probe tree contains symlink {}",
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

fn legacy_attack_markdown(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.is_dir() {
        return Ok(files);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let relative = PathBuf::from(entry.file_name()).join("attack.md");
        if root.join(&relative).is_file() {
            files.push(relative);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!(
            "corpus-probe-migration-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        Store::new(root.join("store"))
    }

    fn legacy_entry(store: &Store, slug: &str) -> PathBuf {
        store.create_project("p", "P", "fixture").unwrap();
        let corpus = store.project_corpus_dir("p");
        fs::remove_dir(corpus.join(PROBES)).unwrap();
        let entry = corpus.join(LEGACY_ATTACKS).join(slug);
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("attack.md"), "probe body\n").unwrap();
        fs::write(entry.join("run.sh"), "#!/bin/sh\n").unwrap();
        entry
    }

    #[test]
    fn migration_previews_applies_and_is_idempotent() {
        let store = rig("happy");
        let entry = legacy_entry(&store, "replay");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(entry.join("run.sh"), fs::Permissions::from_mode(0o751)).unwrap();
        }

        let preview = store.migrate_project_probes("p", false).unwrap();
        assert!(!preview.applied);
        assert_eq!(preview.actions.len(), 2);
        assert!(store
            .project_corpus_dir("p")
            .join("attacks/replay/attack.md")
            .is_file());

        let applied = store.migrate_project_probes("p", true).unwrap();
        assert!(applied.applied);
        let corpus = store.project_corpus_dir("p");
        assert!(!corpus.join(LEGACY_ATTACKS).exists());
        assert_eq!(
            fs::read_to_string(corpus.join("probes/replay/probe.md")).unwrap(),
            "probe body\n"
        );
        assert!(corpus.join("probes/replay/run.sh").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(corpus.join("probes/replay/run.sh"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o751
            );
        }
        assert!(!store.migrate_project_probes("p", true).unwrap().changed());
    }

    #[test]
    fn migration_refuses_mixed_and_malformed_layouts_before_writing() {
        let store = rig("collision");
        let legacy = legacy_entry(&store, "old");
        let current = store.project_corpus_dir("p").join("probes/new");
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("probe.md"), "new\n").unwrap();
        assert!(store.migrate_project_probes("p", true).is_err());
        assert!(legacy.join("attack.md").is_file());
        assert!(current.join("probe.md").is_file());

        fs::remove_dir_all(store.project_corpus_dir("p").join(PROBES)).unwrap();
        fs::remove_file(legacy.join("run.sh")).unwrap();
        assert!(store.migrate_project_probes("p", true).is_err());
        assert!(legacy.join("attack.md").is_file());
    }

    #[test]
    fn migration_handles_empty_compatibility_directories() {
        let store = rig("empty-directories");
        let legacy = legacy_entry(&store, "replay");
        fs::create_dir(store.project_corpus_dir("p").join(PROBES)).unwrap();
        store.migrate_project_probes("p", true).unwrap();
        assert!(store
            .project_corpus_dir("p")
            .join("probes/replay/probe.md")
            .is_file());
        assert!(!legacy.exists());

        fs::create_dir(store.project_corpus_dir("p").join(LEGACY_ATTACKS)).unwrap();
        store.migrate_project_probes("p", true).unwrap();
        assert!(!store.project_corpus_dir("p").join(LEGACY_ATTACKS).exists());
    }

    #[test]
    fn migration_resumes_after_entry_metadata_was_already_renamed() {
        let store = rig("partial-entry");
        let entry = legacy_entry(&store, "replay");
        fs::rename(entry.join("attack.md"), entry.join("probe.md")).unwrap();

        let preview = store.migrate_project_probes("p", false).unwrap();
        assert_eq!(
            preview.actions,
            [format!("rename {LEGACY_ATTACKS}/ to {PROBES}/")]
        );
        store.migrate_project_probes("p", true).unwrap();
        assert!(store
            .project_corpus_dir("p")
            .join("probes/replay/probe.md")
            .is_file());
    }

    #[cfg(unix)]
    #[test]
    fn migration_refuses_symlinks() {
        let store = rig("symlink");
        let legacy = legacy_entry(&store, "linked");
        std::os::unix::fs::symlink("/tmp", legacy.join("evidence")).unwrap();
        let error = store
            .migrate_project_probes("p", false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("symlink"), "{error}");
    }
}
