//! `corpus project/agent/mission/store` admin commands: the scoped
//! store (projects, agents, missions, corpus) exposed headlessly.

use corpus_core::{Mission, Store};

/// `corpus project ...`
pub fn project_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    match args.first().map(String::as_str) {
        Some("list") => {
            for (slug, project) in store.list_projects().map_err(|e| e.to_string())? {
                println!(
                    "{:<20} {:<24} plugin={} created={} gen={}{}",
                    slug,
                    project.name,
                    project.plugin,
                    project.created,
                    project.corpus_generation,
                    project
                        .cloned_from
                        .map(|f| format!(" cloned-from={f}"))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }
        Some("new") => {
            let slug = args.get(1).ok_or("usage: corpus project new <slug> [--name <name>] [--plugin <plugin>]")?;
            let mut name: Option<String> = None;
            let mut plugin = "cdk-regtest".to_string();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--name" => {
                        name = Some(args.get(i + 1).ok_or("missing value after --name")?.clone());
                        i += 2;
                    }
                    "--plugin" => {
                        plugin = args.get(i + 1).ok_or("missing value after --plugin")?.clone();
                        i += 2;
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            let project = store
                .create_project(slug, name.as_deref().unwrap_or(slug), &plugin)
                .map_err(|e| e.to_string())?;
            println!("created project {slug} (plugin: {})", project.plugin);
            Ok(())
        }
        Some("clone") => {
            let from = args
                .get(1)
                .ok_or("usage: corpus project clone <slug> --to <new-slug> [--with-corpus] [--name <name>]")?;
            let mut to: Option<String> = None;
            let mut name: Option<String> = None;
            let mut with_corpus = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--to" => {
                        to = Some(args.get(i + 1).ok_or("missing value after --to")?.clone());
                        i += 2;
                    }
                    "--with-corpus" => {
                        with_corpus = true;
                        i += 1;
                    }
                    "--name" => {
                        name = Some(args.get(i + 1).ok_or("missing value after --name")?.clone());
                        i += 2;
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            let to = to.ok_or("missing required --to <new-slug>")?;
            store
                .clone_project(from, &to, name.as_deref(), with_corpus)
                .map_err(|e| e.to_string())?;
            println!("cloned project {from} -> {to}");
            Ok(())
        }
        Some("delete") => {
            let slug = args.get(1).ok_or("usage: corpus project delete <slug>")?;
            store.delete_project(slug).map_err(|e| e.to_string())?;
            println!("deleted project {slug}");
            Ok(())
        }
        Some("wipe") => {
            let slug = args.get(1).ok_or("usage: corpus project wipe <slug>")?;
            let project = store.wipe_project_corpus(slug).map_err(|e| e.to_string())?;
            println!(
                "wiped project corpus {slug} (generation {})",
                project.corpus_generation
            );
            Ok(())
        }
        Some("rebind") => {
            let slug = args
                .get(1)
                .ok_or("usage: corpus project rebind <slug> --plugin <name>")?;
            let mut plugin: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--plugin" => {
                        plugin = Some(args.get(i + 1).ok_or("missing value after --plugin")?.clone());
                        i += 2;
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            let plugin = plugin.ok_or("missing required --plugin <name>")?;
            store.rebind_project(slug, &plugin).map_err(|e| e.to_string())?;
            println!("rebound project {slug} -> plugin {plugin}");
            Ok(())
        }
        _ => Err(
            "usage: corpus project list|new <slug>|clone <slug> --to <new>|delete <slug>|wipe <slug>|rebind <slug> --plugin <name>"
                .to_string(),
        ),
    }
}

/// `corpus agent ...`
pub fn agent_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    let sub = args.first().map(String::as_str).ok_or_else(|| {
        "usage: corpus agent list|new|clone|delete <project> ...".to_string()
    })?;
    let project = args.get(1).ok_or("missing project slug")?;

    match sub {
        "list" => {
            for (slug, agent) in store.list_agents(project).map_err(|e| e.to_string())? {
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
                        .map(|f| format!(" cloned-from={f}"))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }
        "new" => {
            let slug = args.get(2).ok_or("usage: corpus agent new <project> <slug> [--seed <seed-agent>]")?;
            let mut seed: Option<String> = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--seed" => {
                        seed = Some(args.get(i + 1).ok_or("missing value after --seed")?.clone());
                        i += 2;
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            if let Some(ref s) = seed {
                store.create_agent_from_seed(project, slug, s).map_err(|e| e.to_string())?;
            } else {
                store.create_blank_agent(project, slug).map_err(|e| e.to_string())?;
            }
            println!("created agent {project}/{slug}");
            Ok(())
        }
        "clone" => {
            let from = args.get(2).ok_or("usage: corpus agent clone <project> <from> --to <new-slug>")?;
            let to = args
                .iter()
                .position(|a| a == "--to")
                .and_then(|i| args.get(i + 1))
                .ok_or("missing --to <new-slug>")?
                .clone();
            store.clone_agent(project, from, &to).map_err(|e| e.to_string())?;
            println!("cloned agent {project}/{from} -> {to}");
            Ok(())
        }
        "delete" => {
            let slug = args.get(2).ok_or("usage: corpus agent delete <project> <slug>")?;
            store.delete_agent(project, slug).map_err(|e| e.to_string())?;
            println!("deleted agent {project}/{slug}");
            Ok(())
        }
        "role" => {
            let slug = args
                .get(2)
                .ok_or("usage: corpus agent role <project> <slug> [<researcher|tester|super>]")?;
            match args.get(3) {
                None => {
                    let config = store.load_agent(project, slug).map_err(|e| e.to_string())?;
                    let assigned = if config.meta.has_role() { "" } else { " (unassigned — defaults)" };
                    println!("{project}/{slug}: {}{assigned}", config.meta.role().as_str());
                }
                Some(raw) => {
                    let role = corpus_core::AgentRole::parse(raw).ok_or_else(|| {
                        format!("unknown role {raw:?} — one of researcher|tester|super")
                    })?;
                    store.set_agent_role(project, slug, role).map_err(|e| e.to_string())?;
                    println!("{project}/{slug}: role -> {}", role.as_str());
                }
            }
            Ok(())
        }
        // Assign roles to agents that predate the role system, inferring
        // each from what its permission block already grants. Dry run by
        // default: a capability change is reviewed before it is written.
        "migrate-roles" => {
            let apply = args.iter().any(|a| a == "--apply");
            let rows = store
                .migrate_agent_roles(project, apply)
                .map_err(|e| e.to_string())?;
            if rows.is_empty() {
                println!("no agents in project {project}");
                return Ok(());
            }
            println!("{:<24} {:<14} {:<10} {}", "AGENT", "CURRENT", "INFERRED", "ACTION");
            for row in &rows {
                let current = match row.current {
                    Some(r) => r.as_str().to_string(),
                    None => "—".to_string(),
                };
                let action = if row.applied {
                    "assigned"
                } else if row.current.is_some() {
                    "kept (already assigned)"
                } else {
                    "would assign"
                };
                let flag = if row.needs_review { "  ⚠ no permission block — verify" } else { "" };
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
        _ => Err(
            "usage: corpus agent list|new|clone|delete|role|migrate-roles <project> [<slug>] ..."
                .to_string(),
        ),
    }
}

/// `corpus mission ...`
pub fn mission_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    let sub = args.first().map(String::as_str).ok_or_else(|| {
        "usage: corpus mission list|new|delete <project> ...".to_string()
    })?;
    let project = args.get(1).ok_or("missing project slug")?;

    match sub {
        "list" => {
            for (slug, mission) in store.list_missions(project).map_err(|e| e.to_string())? {
                println!(
                    "{:<20} agent={} status={} budget={} created={} pins={:?}",
                    slug,
                    mission.agent,
                    mission.status,
                    mission.budget.as_deref().unwrap_or("-"),
                    mission.created,
                    mission.pins
                );
            }
            Ok(())
        }
        "new" => {
            let slug = args.get(2).ok_or("usage: corpus mission new <project> <slug> --agent <agent> [--budget <val>] [--pin <repo=rev>] <brief>")?;
            let mut agent: Option<String> = None;
            let mut budget: Option<String> = None;
            let mut pins = std::collections::BTreeMap::new();
            let mut brief_words: Vec<String> = Vec::new();
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--agent" => {
                        agent = Some(args.get(i + 1).ok_or("missing value after --agent")?.clone());
                        i += 2;
                    }
                    "--budget" => {
                        budget = Some(args.get(i + 1).ok_or("missing value after --budget")?.clone());
                        i += 2;
                    }
                    "--pin" => {
                        let spec = args.get(i + 1).ok_or("missing value after --pin")?.clone();
                        if let Some((repo, rev)) = spec.split_once('=') {
                            pins.insert(repo.to_string(), rev.to_string());
                        } else {
                            return Err("--pin expects repo=rev".to_string());
                        }
                        i += 2;
                    }
                    word => {
                        brief_words.push(word.to_string());
                        i += 1;
                    }
                }
            }
            let agent = agent.ok_or("missing required --agent <agent>")?;
            let mission = Mission {
                agent,
                pins,
                budget,
                status: "queued".to_string(),
                created: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                name: None,
                session: None,
                opencode_session: None,
            };
            store
                .write_mission(project, slug, &mission, &brief_words.join(" "))
                .map_err(|e| e.to_string())?;
            println!("created mission {project}/{slug}");
            Ok(())
        }
        "delete" => {
            let slug = args.get(2).ok_or("usage: corpus mission delete <project> <slug>")?;
            store.delete_mission(project, slug).map_err(|e| e.to_string())?;
            println!("deleted mission {project}/{slug}");
            Ok(())
        }
        _ => Err(
            "usage: corpus mission list|new|delete <project> [<slug>] ...".to_string(),
        ),
    }
}

/// `corpus store migrate [--dry-run] [--project <slug>] [--confirm]`
pub fn store_cmd(args: &[String]) -> Result<(), String> {
    let mut project = corpus_core::DEFAULT_PROJECT_SLUG.to_string();
    let mut dry_run = false;
    let confirm = args.iter().any(|a| a == "--confirm");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "migrate" => i += 1,
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--project" => {
                project = args.get(i + 1).ok_or("missing value after --project")?.clone();
                i += 2;
            }
            "--confirm" => {
                i += 1;
            }
            other => return Err(format!("unknown store option: {other}")),
        }
    }
    let store = Store::from_env();
    // v2 migration: if confirm is passed, also remove legacy template
    // directories (the old permissions/prompts tiers).
    if confirm {
        let tpl = store.root().join("templates");
        for kind in ["permissions", "prompts"] {
            let dir = tpl.join(kind);
            if dir.is_dir() {
                println!("removing legacy template dir {}...", dir.display());
                std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            }
        }
        // Also remove per-project template dirs if they exist.
        if let Ok(projects) = store.list_projects() {
            for (slug, _) in projects {
                let pt = store.project_dir(&slug).join("templates");
                if pt.is_dir() {
                    let _ = std::fs::remove_dir_all(&pt);
                    println!("removed project {slug} legacy templates");
                }
            }
        }
    }
    let report = store
        .migrate_legacy_flat_opt(&project, corpus_core::MigrateOptions { dry_run })
        .map_err(|e| e.to_string())?;
    if report.dry_run {
        println!(
            "dry run: no changes made; {} entrie(s) would move into projects/{project}/corpus/",
            report.would_move.len()
        );
        for entry in &report.would_move {
            println!("  would move {:?}", entry.display());
        }
        return Ok(());
    }
    println!("migrated flat store into projects/{project}/corpus/");
    for moved in &report.moved {
        let checksum = corpus_core::checksum(moved).map_err(|e| e.to_string())?;
        println!("  moved {:?} fnv1a={checksum}", moved.display());
    }
    for skipped in &report.skipped {
        println!("  skipped (destination present, never overwritten) {:?}", skipped.display());
    }
    for unverified in &report.unverified {
        eprintln!(
            "  ** UNVERIFIED (post-move checksum mismatch) {:?} — fetch this back up",
            unverified.display()
        );
    }
    for category in &report.removed_categories {
        println!("  removed legacy {category}/ (all entries verified)");
    }
    if report.unverified.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "migration produced {} unverified entrie(s); legacy category dirs were \
             kept in place for them",
            report.unverified.len()
        ))
    }
}