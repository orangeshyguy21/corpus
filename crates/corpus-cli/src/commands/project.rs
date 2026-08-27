//! Project-store CLI commands.

use corpus_core::Store;

use crate::cli::ProjectCommand;

pub(crate) fn run(command: ProjectCommand) -> Result<(), String> {
    let store = Store::from_env();
    match command {
        ProjectCommand::List => list(&store),
        ProjectCommand::New { slug, name, plugin } => new(&store, &slug, name.as_deref(), &plugin),
        ProjectCommand::Clone {
            slug,
            to,
            with_corpus,
            name,
        } => clone(&store, &slug, &to, name.as_deref(), with_corpus),
        ProjectCommand::Delete { slug } => delete(&store, &slug),
        ProjectCommand::Wipe { slug } => wipe(&store, &slug),
        ProjectCommand::Rebind { slug, plugin } => rebind(&store, &slug, &plugin),
        ProjectCommand::MigrateProbes { project, apply } => migrate_probes(&store, &project, apply),
    }
}

fn list(store: &Store) -> Result<(), String> {
    for (slug, project) in store.list_projects().map_err(|error| error.to_string())? {
        println!(
            "{:<20} {:<24} plugin={} created={} gen={}{}",
            slug,
            project.name,
            project.plugin,
            project.created,
            project.corpus_generation,
            project
                .cloned_from
                .map(|from| format!(" cloned-from={from}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn new(store: &Store, slug: &str, name: Option<&str>, plugin: &str) -> Result<(), String> {
    let project = store
        .create_project(slug, name.unwrap_or(slug), plugin)
        .map_err(|error| error.to_string())?;
    println!("created project {slug} (plugin: {})", project.plugin);
    Ok(())
}

fn clone(
    store: &Store,
    from: &str,
    to: &str,
    name: Option<&str>,
    with_corpus: bool,
) -> Result<(), String> {
    store
        .clone_project(from, to, name, with_corpus)
        .map_err(|error| error.to_string())?;
    println!("cloned project {from} -> {to}");
    Ok(())
}

fn delete(store: &Store, slug: &str) -> Result<(), String> {
    let missions = store
        .list_missions(slug)
        .map_err(|error| error.to_string())?;
    if missions
        .iter()
        .any(|(mission, _)| store.ensure_mission_deletable(slug, mission).is_err())
    {
        store
            .request_project_delete(slug)
            .map_err(|error| error.to_string())?;
        println!("requested deletion of project {slug}; the app will tear down its missions first");
    } else {
        store
            .delete_project(slug)
            .map_err(|error| error.to_string())?;
        println!("deleted project {slug}");
    }
    Ok(())
}

fn wipe(store: &Store, slug: &str) -> Result<(), String> {
    let project = store
        .wipe_project_corpus(slug)
        .map_err(|error| error.to_string())?;
    println!(
        "wiped project corpus {slug} (generation {})",
        project.corpus_generation
    );
    Ok(())
}

fn rebind(store: &Store, slug: &str, plugin: &str) -> Result<(), String> {
    store
        .rebind_project(slug, plugin)
        .map_err(|error| error.to_string())?;
    println!("rebound project {slug} -> plugin {plugin}");
    Ok(())
}

fn migrate_probes(store: &Store, project: &str, apply: bool) -> Result<(), String> {
    let migration = store
        .migrate_project_probes(project, apply)
        .map_err(|error| error.to_string())?;
    if !migration.changed() {
        println!("project {project}: probe namespace already current");
        return Ok(());
    }
    println!(
        "project {project}: {}",
        if apply { "applied" } else { "dry run" }
    );
    for action in migration.actions {
        println!("  {action}");
    }
    if !apply {
        println!("re-run with --apply to perform this migration");
    }
    Ok(())
}
