//! Failure artifact capture for integration scenarios.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub struct ArtifactBundle {
    scenario: String,
    run_id: String,
    staging: PathBuf,
    destination: PathBuf,
    preserved: bool,
}

impl ArtifactBundle {
    pub fn new(scenario: &str, run_id: &str, scratch: &Path) -> io::Result<Self> {
        let staging = scratch.join("evidence");
        fs::create_dir_all(&staging)?;
        let root = std::env::var_os("CORPUS_INTEGRATION_ARTIFACTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root().join("target/corpus-integration-artifacts"));
        Ok(Self {
            scenario: scenario.to_string(),
            run_id: run_id.to_string(),
            staging,
            destination: root.join(run_id),
            preserved: false,
        })
    }

    pub fn write_text(&self, relative: &str, contents: &str) -> io::Result<()> {
        let path = self.staging.join(relative);
        ensure_relative(relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)
    }

    pub fn write_json<T: Serialize>(&self, relative: &str, value: &T) -> io::Result<()> {
        let json = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
        self.write_text(relative, &json)
    }

    pub fn preserve(&mut self, world: &Path, failure: &str) -> io::Result<PathBuf> {
        self.write_text("failure.txt", failure)?;
        self.write_text(
            "scenario.txt",
            &format!("scenario={}\nrun_id={}\n", self.scenario, self.run_id),
        )?;
        if self.destination.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "artifact destination exists: {}",
                    self.destination.display()
                ),
            ));
        }
        fs::create_dir_all(&self.destination)?;
        copy_tree(&self.staging, &self.destination)?;
        let store = world.join("store");
        if store.exists() {
            copy_tree(&store, &self.destination.join("store-after"))?;
        }
        self.preserved = true;
        Ok(self.destination.clone())
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }

    pub fn was_preserved(&self) -> bool {
        self.preserved
    }
}

fn ensure_relative(path: &str) -> io::Result<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact path must stay inside the bundle",
        ));
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&source, &target)?;
        } else if file_type.is_file() {
            fs::copy(source, target)?;
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("integration crate is under workspace/crates")
        .to_path_buf()
}
