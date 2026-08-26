//! Project records and project-scoped lifecycle mutations.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filesystem::atomic_write;
use crate::missions::MissionDeleteRequest;
use crate::store::{now_epoch, validate_slug, Store, CATEGORIES};
use crate::yaml;

/// A project (`store/projects/<slug>/project.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// The environment plugin this project is bound to.
    pub plugin: String,
    /// Epoch seconds of creation.
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
    /// Bumped on every corpus wipe so old run logs stay attributable.
    #[serde(default)]
    pub corpus_generation: u64,
    /// The project's chosen source revs (`repo -> rev`, edited in the top
    /// bar): the revs available come from the plugin, the selection is the
    /// project's. Missions stamp these at creation. Empty means every source
    /// remains at its default rev.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, String>,
    /// Durable lifecycle intent consumed by the desktop app. The project
    /// remains present until every mission environment has been torn down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_requested: Option<MissionDeleteRequest>,
}

impl Project {
    pub fn load(store: &Store, slug: &str) -> Result<Self> {
        validate_slug(slug)?;
        let path = store.project_dir(slug).join("project.yaml");
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("project not found: {slug}")))?;
        yaml::from_str(&raw).map_err(|error| Error::Store(format!("project {slug}: {error}")))
    }

    fn save(&self, store: &Store, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let path = store.project_dir(slug).join("project.yaml");
        atomic_write(&path, yaml::to_string(self)?)
    }
}

impl Store {
    /// Create an empty project: `store/projects/<slug>/` with corpus category,
    /// agent, and mission directories. Agents are added explicitly by role.
    pub fn create_project(&self, slug: &str, name: &str, plugin: &str) -> Result<Project> {
        validate_slug(slug)?;
        let dir = self.project_dir(slug);
        if dir.exists() {
            return Err(Error::Store(format!("project already exists: {slug}")));
        }
        fs::create_dir_all(&dir)?;
        let project = Project {
            name: name.to_string(),
            plugin: plugin.to_string(),
            created: now_epoch(),
            cloned_from: None,
            corpus_generation: 0,
            pins: BTreeMap::new(),
            delete_requested: None,
        };
        fs::create_dir_all(self.project_agents_dir(slug))?;
        fs::create_dir_all(self.project_missions_dir(slug))?;
        for category in CATEGORIES {
            fs::create_dir_all(self.project_corpus_dir(slug).join(category))?;
        }
        project.save(self, slug)?;
        Ok(project)
    }

    /// List projects, sorted by slug.
    pub fn list_projects(&self) -> Result<Vec<(String, Project)>> {
        let mut found = Vec::new();
        let dir = self.projects_dir();
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::Store("non-utf8 project dir".into()))?;
            if let Ok(project) = Project::load(self, slug) {
                found.push((slug.to_string(), project));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// Delete a project only after every mission environment is closed.
    pub fn delete_project(&self, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_dir(slug);
        if !dir.is_dir() {
            return Err(Error::Store(format!("project not found: {slug}")));
        }
        for (mission, _) in self.list_missions(slug)? {
            self.ensure_mission_deletable(slug, &mission)?;
        }
        fs::remove_dir_all(&dir)?;
        let _ = fs::remove_dir_all(self.project_run_dir(slug));
        Ok(())
    }

    /// Persist a project deletion request for the app lifecycle reconciler.
    pub fn request_project_delete(&self, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let mut project = Project::load(self, slug)?;
        project
            .delete_requested
            .get_or_insert(MissionDeleteRequest {
                requested_at: now_epoch(),
            });
        project.save(self, slug)
    }

    /// Clone a project: config + agents + missions, corpus copy optional.
    pub fn clone_project(
        &self,
        from: &str,
        to: &str,
        name: Option<&str>,
        with_corpus: bool,
    ) -> Result<Project> {
        let source = Project::load(self, from)?;
        validate_slug(to)?;
        if source.delete_requested.is_some()
            || self
                .list_agents(from)?
                .iter()
                .any(|(_, agent)| agent.meta.delete_requested.is_some())
        {
            return Err(Error::Store(format!(
                "project {from} or one of its agents is pending deletion"
            )));
        }
        let agents = self.project_agents_dir(from);
        let missions = self.project_missions_dir(from);
        let corpus = self.project_corpus_dir(from);
        validate_copy_tree(&agents)?;
        validate_copy_tree(&missions)?;
        if with_corpus {
            validate_copy_tree(&corpus)?;
        }

        // Claim the final destination atomically, but publish project.yaml
        // last. Project discovery and every scoped launch require that file,
        // so a concurrent reader cannot mistake an in-progress tree for a
        // usable project. The guard removes the complete destination on every
        // failure path.
        fs::create_dir_all(self.projects_dir())?;
        let mut pending = PendingProjectDir::create(self.project_dir(to))?;
        fs::create_dir(pending.path.join("agents"))?;
        fs::create_dir(pending.path.join("missions"))?;
        fs::create_dir(pending.path.join("corpus"))?;
        for category in CATEGORIES {
            fs::create_dir(pending.path.join("corpus").join(category))?;
        }

        let project = Project {
            name: name.unwrap_or(&source.name).to_string(),
            cloned_from: Some(from.to_string()),
            delete_requested: None,
            ..source
        };
        copy_tree(&agents, &pending.path.join("agents"))?;
        copy_tree(&missions, &pending.path.join("missions"))?;
        if with_corpus {
            copy_tree(&corpus, &pending.path.join("corpus"))?;
        }
        project.save(self, to)?;
        pending.commit();
        Ok(project)
    }

    /// Rename a project's display name without changing its slug identity.
    pub fn rename_project(&self, slug: &str, name: &str) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Store("project name must not be empty".into()));
        }
        let mut project = Project::load(self, slug)?;
        project.name = name.to_string();
        project.save(self, slug)?;
        Ok(project)
    }

    /// Rebind a project's environment plugin.
    pub fn rebind_project(&self, slug: &str, plugin: &str) -> Result<Project> {
        let mut project = Project::load(self, slug)?;
        project.plugin = plugin.to_string();
        project.save(self, slug)?;
        Ok(project)
    }

    /// Persist a project's source-revision selection.
    pub fn set_project_pins(&self, slug: &str, pins: BTreeMap<String, String>) -> Result<Project> {
        let mut project = Project::load(self, slug)?;
        project.pins = pins;
        project.save(self, slug)?;
        Ok(project)
    }

    /// Wipe the working corpus and advance its provenance generation.
    pub fn wipe_project_corpus(&self, slug: &str) -> Result<Project> {
        let mut project = Project::load(self, slug)?;
        let corpus = self.project_corpus_dir(slug);
        if corpus.is_dir() {
            fs::remove_dir_all(&corpus)?;
        }
        ensure_corpus_categories(&corpus)?;
        project.corpus_generation += 1;
        project.save(self, slug)?;
        Ok(project)
    }
}

fn ensure_corpus_categories(corpus: &Path) -> Result<()> {
    for category in CATEGORIES {
        fs::create_dir_all(corpus.join(category))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "projects/tests.rs"]
mod tests;

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let root = fs::symlink_metadata(src)?;
    if root.file_type().is_symlink() || !root.is_dir() {
        return Err(Error::Store(format!(
            "project clone source must be a real directory: {}",
            src.display()
        )));
    }
    if !dst.is_dir() {
        fs::create_dir(dst)?;
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            return Err(Error::Store(format!(
                "project clone refuses symlink {}",
                from.display()
            )));
        }
        if metadata.is_dir() {
            match fs::symlink_metadata(&to) {
                Ok(existing) if existing.is_dir() && !existing.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(Error::Store(format!(
                        "project clone destination is not a real directory: {}",
                        to.display()
                    )))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&to)?;
                }
                Err(error) => return Err(error.into()),
            }
            copy_tree(&from, &to)?;
        } else if metadata.is_file() {
            fs::copy(&from, &to)?;
        } else {
            return Err(Error::Store(format!(
                "project clone refuses special file {}",
                from.display()
            )));
        }
    }
    Ok(())
}

fn validate_copy_tree(root: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::Store(format!(
            "project clone source must be a real directory: {}",
            root.display()
        )));
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(Error::Store(format!(
                    "project clone refuses symlink {}",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                stack.push(path);
            } else if !metadata.is_file() {
                return Err(Error::Store(format!(
                    "project clone refuses special file {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

struct PendingProjectDir {
    path: std::path::PathBuf,
    committed: bool,
}

impl PendingProjectDir {
    fn create(path: std::path::PathBuf) -> Result<Self> {
        fs::create_dir(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Store(format!("project already exists: {}", path.display()))
            } else {
                error.into()
            }
        })?;
        Ok(Self {
            path,
            committed: false,
        })
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PendingProjectDir {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
