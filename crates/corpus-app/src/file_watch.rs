//! Coarse store invalidation from the platform filesystem watcher.
//!
//! Events are hints, never state: backstop reconciliation remains in
//! `AppState`. A callback coalesces into bounded project/kind sets and wakes
//! egui; it never reads the store or assigns an event to one run.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::jobs::RepaintWake;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct FileInvalidations {
    pub project_index: bool,
    pub all_projects: bool,
    pub metadata: BTreeSet<String>,
    pub corpus: BTreeSet<String>,
    pub activity: BTreeSet<String>,
    pub warning: Option<String>,
}

impl FileInvalidations {
    pub(crate) fn is_empty(&self) -> bool {
        !self.project_index
            && !self.all_projects
            && self.metadata.is_empty()
            && self.corpus.is_empty()
            && self.activity.is_empty()
            && self.warning.is_none()
    }

    fn merge_path(&mut self, projects_root: &Path, path: &Path) {
        let Ok(relative) = path.strip_prefix(projects_root) else {
            self.invalidate_all();
            return;
        };
        let parts = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>();
        let Some(project) = parts.first().copied().filter(|value| !value.is_empty()) else {
            self.invalidate_all();
            return;
        };
        match parts.get(1).copied() {
            None => {
                self.project_index = true;
                self.metadata.insert(project.into());
                self.corpus.insert(project.into());
            }
            Some("project.yaml") => {
                self.project_index = true;
                self.metadata.insert(project.into());
            }
            Some("agents" | "missions") => {
                self.metadata.insert(project.into());
            }
            Some("corpus") => {
                if parts.get(2) == Some(&"runs")
                    && parts.last().is_some_and(|name| name.ends_with(".raw"))
                {
                    self.activity.insert(project.into());
                } else {
                    self.corpus.insert(project.into());
                }
            }
            Some(_) => {
                // Unknown project-local paths can still affect a future
                // view. Reconcile metadata rather than guessing semantics.
                self.metadata.insert(project.into());
            }
        }
    }

    fn invalidate_all(&mut self) {
        self.project_index = true;
        self.all_projects = true;
    }
}

pub(crate) trait FileInvalidationSource {
    fn take(&self) -> FileInvalidations;
}

pub(crate) struct NotifyFileInvalidationSource {
    _watcher: RecommendedWatcher,
    pending: Arc<Mutex<FileInvalidations>>,
}

impl NotifyFileInvalidationSource {
    pub(crate) fn new(projects_root: PathBuf, wake: Arc<dyn RepaintWake>) -> Result<Self, String> {
        std::fs::create_dir_all(&projects_root)
            .map_err(|error| format!("cannot create store watch root: {error}"))?;
        let pending = Arc::new(Mutex::new(FileInvalidations::default()));
        let callback_pending = pending.clone();
        let callback_root = projects_root.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let mut wake_needed = true;
                if let Ok(mut invalidations) = callback_pending.lock() {
                    match event {
                        // A reconciliation read must never invalidate itself
                        // on backends that surface open/close/read access.
                        Ok(event) if matches!(event.kind, EventKind::Access(_)) => {
                            wake_needed = false;
                        }
                        Ok(event) if event.paths.is_empty() => invalidations.invalidate_all(),
                        Ok(event) => {
                            for path in event.paths {
                                invalidations.merge_path(&callback_root, &path);
                            }
                        }
                        Err(error) => {
                            invalidations.invalidate_all();
                            if invalidations.warning.is_none() {
                                invalidations.warning =
                                    Some(format!("filesystem watcher degraded: {error}"));
                            }
                        }
                    }
                }
                if wake_needed {
                    wake.request_repaint();
                }
            })
            .map_err(|error| format!("cannot create filesystem watcher: {error}"))?;
        watcher
            .watch(&projects_root, RecursiveMode::Recursive)
            .map_err(|error| format!("cannot watch {}: {error}", projects_root.display()))?;
        Ok(Self {
            _watcher: watcher,
            pending,
        })
    }
}

impl FileInvalidationSource for NotifyFileInvalidationSource {
    fn take(&self) -> FileInvalidations {
        let Ok(mut pending) = self.pending.lock() else {
            return FileInvalidations {
                project_index: true,
                all_projects: true,
                warning: Some("filesystem watcher state was poisoned; using reconciliation".into()),
                ..FileInvalidations::default()
            };
        };
        std::mem::take(&mut *pending)
    }
}

#[cfg(test)]
pub(crate) struct FakeFileInvalidationSource {
    pending: Mutex<FileInvalidations>,
}

#[cfg(test)]
impl FakeFileInvalidationSource {
    pub(crate) fn new(pending: FileInvalidations) -> Self {
        Self {
            pending: Mutex::new(pending),
        }
    }
}

#[cfg(test)]
impl FileInvalidationSource for FakeFileInvalidationSource {
    fn take(&self) -> FileInvalidations {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_is_coarse_and_project_scoped() {
        let root = Path::new("/store/projects");
        let mut found = FileInvalidations::default();
        found.merge_path(root, Path::new("/store/projects/p/corpus/runs/1-a.raw"));
        found.merge_path(root, Path::new("/store/projects/p/corpus/findings/f.md"));
        found.merge_path(root, Path::new("/store/projects/p/missions/m/mission.yaml"));
        assert_eq!(found.activity, BTreeSet::from(["p".into()]));
        assert_eq!(found.corpus, BTreeSet::from(["p".into()]));
        assert_eq!(found.metadata, BTreeSet::from(["p".into()]));
        assert!(!found.project_index);
    }

    #[test]
    fn directory_level_or_out_of_root_events_force_full_reconciliation() {
        let root = Path::new("/store/projects");
        let mut found = FileInvalidations::default();
        found.merge_path(root, root);
        assert!(found.project_index && found.all_projects);

        let mut outside = FileInvalidations::default();
        outside.merge_path(root, Path::new("/other/path"));
        assert!(outside.project_index && outside.all_projects);
    }

    #[test]
    fn raw_capture_is_never_mapped_to_a_run() {
        let root = Path::new("/store/projects");
        let mut found = FileInvalidations::default();
        found.merge_path(root, Path::new("/store/projects/p/corpus/runs/1-a.raw"));
        assert_eq!(found.activity, BTreeSet::from(["p".into()]));
        assert!(found.metadata.is_empty());
        assert!(found.corpus.is_empty());
    }
}
