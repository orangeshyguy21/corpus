//! Mission-store CLI commands.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use corpus_core::{Mission, MissionDeleteRequest, Project, Store};

use crate::cli::{MissionCommand, SourcePin};

pub(crate) fn run(command: MissionCommand) -> Result<(), String> {
    let store = Store::from_env();
    match command {
        MissionCommand::List { project } => list(&store, &project),
        MissionCommand::New {
            project,
            slug,
            agent,
            budget,
            pins,
            brief,
        } => new(
            &store,
            NewMission {
                project,
                slug,
                agent,
                budget,
                overrides: pins,
                brief,
            },
        ),
        MissionCommand::Delete { project, slug } => delete(&store, &project, &slug),
    }
}

fn list(store: &Store, project: &str) -> Result<(), String> {
    for (slug, mission) in store
        .list_missions(project)
        .map_err(|error| error.to_string())?
    {
        println!(
            "{:<20} agent={} budget={} created={} pins={:?}",
            slug,
            mission.agent,
            mission.budget.as_deref().unwrap_or("-"),
            mission.created,
            mission.pins
        );
    }
    Ok(())
}

struct NewMission {
    project: String,
    slug: String,
    agent: String,
    budget: Option<String>,
    overrides: Vec<SourcePin>,
    brief: Vec<String>,
}

fn new(store: &Store, request: NewMission) -> Result<(), String> {
    let NewMission {
        project,
        slug,
        agent,
        budget,
        overrides,
        brief,
    } = request;
    // Missions stamp the project's effective source selection. A stored
    // project pin overrides the plugin default; the command-line overrides
    // are the final per-mission selection.
    let plugin_defaults = corpus_core::plugin_sources(store, &project)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|source| {
            let revision = source.default_rev().to_string();
            (source.name, revision)
        });
    let project_pins = Project::load(store, &project)
        .map_err(|error| error.to_string())?
        .pins;
    let pins = merge_pins(plugin_defaults, project_pins, overrides);

    // Reject a rev that could never resolve at authoring time, not launch.
    for (source, revision) in &pins {
        corpus_core::validate_pin(store, &project, source, revision)
            .map_err(|error| error.to_string())?;
    }
    let mission = Mission {
        agent,
        pins,
        budget,
        created: now_epoch(),
        name: None,
        session: None,
        control: None,
        opencode_session: None,
        opencode_workspace: None,
        environment_session: None,
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    };
    store
        .write_mission(&project, &slug, &mission, &brief.join(" "))
        .map_err(|error| error.to_string())?;
    println!("created mission {project}/{slug}");
    Ok(())
}

fn merge_pins(
    plugin_defaults: impl IntoIterator<Item = (String, String)>,
    project_pins: BTreeMap<String, String>,
    overrides: Vec<SourcePin>,
) -> BTreeMap<String, String> {
    let mut pins: BTreeMap<_, _> = plugin_defaults.into_iter().collect();
    pins.extend(project_pins);
    pins.extend(overrides.into_iter().map(|pin| (pin.source, pin.revision)));
    pins
}

fn delete(store: &Store, project: &str, slug: &str) -> Result<(), String> {
    if store.ensure_mission_deletable(project, slug).is_ok() {
        store
            .delete_mission(project, slug)
            .map_err(|error| error.to_string())?;
        println!("deleted mission {project}/{slug}");
        return Ok(());
    }
    let mut mission = store
        .load_mission(project, slug)
        .map_err(|error| error.to_string())?;
    mission.launch_requested = None;
    mission
        .delete_requested
        .get_or_insert(MissionDeleteRequest {
            requested_at: now_epoch(),
        });
    store
        .update_mission(project, slug, &mission)
        .map_err(|error| error.to_string())?;
    println!(
        "deletion requested for mission {project}/{slug}; open corpus-app to complete lifecycle teardown"
    );
    Ok(())
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mission_overrides_win_after_project_pins_and_plugin_defaults() {
        let plugin = [
            ("target".to_string(), "plugin".to_string()),
            ("shared".to_string(), "plugin".to_string()),
        ];
        let project = BTreeMap::from([
            ("shared".to_string(), "project".to_string()),
            ("tools".to_string(), "project".to_string()),
        ]);
        let overrides = vec![
            SourcePin {
                source: "tools".into(),
                revision: "mission".into(),
            },
            SourcePin {
                source: "extra".into(),
                revision: "mission".into(),
            },
        ];

        assert_eq!(
            merge_pins(plugin, project, overrides),
            BTreeMap::from([
                ("extra".to_string(), "mission".to_string()),
                ("shared".to_string(), "project".to_string()),
                ("target".to_string(), "plugin".to_string()),
                ("tools".to_string(), "mission".to_string()),
            ])
        );
    }
}
