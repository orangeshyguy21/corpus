//! Shared store and clock adapters used across admin tool domains.

use std::time::{SystemTime, UNIX_EPOCH};

use corpus_store::{Project, Store};

use crate::error::{Error, Result};

/// Load a project or surface a clean tool-argument error.
pub(crate) fn load_project(store: &Store, slug: &str) -> Result<Project> {
    Project::load(store, slug).map_err(|error| Error::Args(error.to_string()))
}

/// Current Unix timestamp used by persisted lifecycle requests and short-lived
/// confirmations. A pre-epoch clock degrades to zero instead of panicking.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
