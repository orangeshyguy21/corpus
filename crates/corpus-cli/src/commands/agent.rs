//! Agent-store CLI commands.

use corpus_core::{AgentRole, Store};

use crate::cli::AgentCommand;

pub(crate) fn run(command: AgentCommand) -> Result<(), String> {
    let store = Store::from_env();
    match command {
        AgentCommand::List { project } => list(&store, &project),
        AgentCommand::New {
            project,
            slug,
            role,
        } => new(&store, &project, &slug, role),
        AgentCommand::Clone { project, from, to } => clone(&store, &project, &from, &to),
        AgentCommand::Delete { project, slug } => delete(&store, &project, &slug),
        AgentCommand::Role {
            project,
            slug,
            role,
        } => manage_role(&store, &project, &slug, role),
        AgentCommand::MigrateRoles { project, apply } => migrate_roles(&store, &project, apply),
    }
}

fn list(store: &Store, project: &str) -> Result<(), String> {
    for (slug, agent) in store
        .list_agents(project)
        .map_err(|error| error.to_string())?
    {
        println!(
            "{:<20} {:<24} created={} hash={}{}",
            slug,
            agent.meta.name,
            agent.meta.created,
            store.agent_config_hash(project, &slug).unwrap_or_default(),
            agent
                .meta
                .cloned_from
                .as_deref()
                .map(|from| format!(" cloned-from={from}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn new(store: &Store, project: &str, slug: &str, role: AgentRole) -> Result<(), String> {
    store
        .create_agent_with_role(project, slug, role)
        .map_err(|error| error.to_string())?;
    println!("created agent {project}/{slug} (role: {})", role.as_str());
    Ok(())
}

fn clone(store: &Store, project: &str, from: &str, to: &str) -> Result<(), String> {
    store
        .clone_agent(project, from, to)
        .map_err(|error| error.to_string())?;
    println!("cloned agent {project}/{from} -> {to}");
    Ok(())
}

fn delete(store: &Store, project: &str, slug: &str) -> Result<(), String> {
    let missions = store
        .missions_for_agent(project, slug)
        .map_err(|error| error.to_string())?;
    if missions
        .iter()
        .any(|mission| store.ensure_mission_deletable(project, mission).is_err())
    {
        store
            .request_agent_delete(project, slug)
            .map_err(|error| error.to_string())?;
        println!(
            "requested deletion of agent {project}/{slug}; the app will tear down its missions first"
        );
    } else {
        store
            .delete_agent(project, slug)
            .map_err(|error| error.to_string())?;
        println!("deleted agent {project}/{slug}");
    }
    Ok(())
}

fn manage_role(
    store: &Store,
    project: &str,
    slug: &str,
    role: Option<AgentRole>,
) -> Result<(), String> {
    if let Some(role) = role {
        store
            .set_agent_role(project, slug, role)
            .map_err(|error| error.to_string())?;
        println!("{project}/{slug}: role -> {}", role.as_str());
    } else {
        let config = store
            .load_agent(project, slug)
            .map_err(|error| error.to_string())?;
        let assigned = if config.meta.has_role() {
            ""
        } else {
            " (unassigned — defaults)"
        };
        println!(
            "{project}/{slug}: {}{assigned}",
            config.meta.role().as_str()
        );
    }
    Ok(())
}

fn migrate_roles(store: &Store, project: &str, apply: bool) -> Result<(), String> {
    let rows = store
        .migrate_agent_roles(project, apply)
        .map_err(|error| error.to_string())?;
    if rows.is_empty() {
        println!("no agents in project {project}");
        return Ok(());
    }
    println!(
        "{:<24} {:<14} {:<10} ACTION",
        "AGENT", "CURRENT", "INFERRED"
    );
    for row in &rows {
        let current = row
            .current
            .map(|role| role.as_str().to_string())
            .unwrap_or_else(|| "—".to_string());
        let action = if row.applied {
            "assigned"
        } else if row.current.is_some() {
            "kept (already assigned)"
        } else {
            "would assign"
        };
        let flag = if row.needs_review {
            "  ⚠ no permission block — verify"
        } else {
            ""
        };
        println!(
            "{:<24} {:<14} {:<10} {action}{flag}",
            row.agent,
            current,
            row.inferred.as_str()
        );
    }
    if !apply {
        println!("\ndry run — re-run with --apply to write these roles");
    }
    Ok(())
}
