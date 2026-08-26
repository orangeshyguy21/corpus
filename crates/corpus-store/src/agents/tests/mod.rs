use super::permissions::{bind_permission, DataRoots, RenderCtx};
use super::rendering::entry_role;
use super::repository::read_sidecar;
use super::roles::PROJECT_MANAGEMENT_TOOLS;
use super::*;
use crate::store::Store;
use std::collections::BTreeSet;
use std::fs;

mod mutations;
mod rendering;
mod repository;
mod roles;

/// A store in its own world — see the note in `launch::tests`: run
/// dirs are siblings of the store, so each test store needs its own
/// parent or they share `<parent>/var/run/<project>`.
fn tmp_store(tag: &str) -> Store {
    let world = std::env::temp_dir().join(format!("corpus-agents-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    Store::new(world.join("store"))
}

fn doc(agent: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "$schema": OPENCODE_SCHEMA, "agent": agent })
}

/// Parse the rendered frontmatter's permission block.
fn rendered_permission(text: &str) -> crate::yaml::Value {
    let fm = text.split("---\n").nth(1).unwrap();
    let yaml: crate::yaml::Value = crate::yaml::from_str(fm).unwrap();
    yaml["permission"].clone()
}
