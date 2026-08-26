//! Isolated world owned by one integration scenario.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use corpus_store::Store;
use serde::Serialize;

use crate::artifacts::ArtifactBundle;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn unique_id() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

pub struct TestHarness {
    scenario: String,
    run_id: String,
    world: PathBuf,
    store: Store,
    artifacts: ArtifactBundle,
}

impl TestHarness {
    pub fn new(scenario: &str) -> Self {
        let run_id = format!("{}-{}", scenario, unique_id());
        let world = std::env::temp_dir().join(format!("corpus-integration-{run_id}"));
        std::fs::create_dir_all(&world).expect("create isolated integration world");
        // OpenCode canonicalizes its session directory. Canonicalize the
        // harness root too so macOS's /var -> /private/var alias cannot make
        // the production directory-ownership check reject the same path.
        let world = world
            .canonicalize()
            .expect("canonicalize isolated integration world");
        let store = Store::new(world.join("store")).with_actor(format!("integration:{scenario}"));
        let artifacts = ArtifactBundle::new(scenario, &run_id, &world)
            .expect("create integration artifact staging");
        Self {
            scenario: scenario.to_string(),
            run_id,
            world,
            store,
            artifacts,
        }
    }

    pub fn scenario(&self) -> &str {
        &self.scenario
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn world(&self) -> &Path {
        &self.world
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn record_text(&self, relative: &str, contents: &str) {
        self.artifacts
            .write_text(relative, contents)
            .expect("write integration evidence");
    }

    pub fn record_json<T: Serialize>(&self, relative: &str, value: &T) {
        self.artifacts
            .write_json(relative, value)
            .expect("write integration evidence");
    }

    pub fn preserve_failure(&mut self, failure: &str) -> PathBuf {
        self.artifacts
            .preserve(&self.world, failure)
            .expect("preserve integration failure artifacts")
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        if std::thread::panicking() && !self.artifacts.was_preserved() {
            let destination = self.artifacts.destination().to_path_buf();
            if let Err(error) = self
                .artifacts
                .preserve(&self.world, "test panicked; inspect captured evidence")
            {
                eprintln!(
                    "corpus integration: could not preserve {}: {error}",
                    destination.display()
                );
            } else {
                eprintln!(
                    "corpus integration: failure artifacts: {}",
                    destination.display()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&self.world);
    }
}
