//! `corpus project/agent/mission/store` admin commands: the scoped
//! store (projects, agents, missions, corpus) exposed headlessly.

use corpus_core::{
    FindingQuery, FindingSeverity, FindingSort, Mission, MissionDeleteRequest, Store,
};

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
    let sub = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| "usage: corpus agent list|new|clone|delete <project> ...".to_string())?;
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
            let usage = format!(
                "usage: corpus agent new <project> <slug> [--role {}]",
                corpus_core::AgentRole::names()
            );
            let slug = args.get(2).ok_or_else(|| usage.clone())?;
            let mut role = corpus_core::AgentRole::Researcher;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--role" => {
                        let raw = args.get(i + 1).ok_or("missing value after --role")?;
                        role = corpus_core::AgentRole::parse(raw)
                            .ok_or_else(|| format!("--role {raw:?}: {usage}"))?;
                        i += 2;
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            store
                .create_agent_with_role(project, slug, role)
                .map_err(|e| e.to_string())?;
            println!("created agent {project}/{slug} (role: {})", role.as_str());
            Ok(())
        }
        "clone" => {
            let from = args
                .get(2)
                .ok_or("usage: corpus agent clone <project> <from> --to <new-slug>")?;
            let to = args
                .iter()
                .position(|a| a == "--to")
                .and_then(|i| args.get(i + 1))
                .ok_or("missing --to <new-slug>")?
                .clone();
            store
                .clone_agent(project, from, &to)
                .map_err(|e| e.to_string())?;
            println!("cloned agent {project}/{from} -> {to}");
            Ok(())
        }
        "delete" => {
            let slug = args
                .get(2)
                .ok_or("usage: corpus agent delete <project> <slug>")?;
            store
                .delete_agent(project, slug)
                .map_err(|e| e.to_string())?;
            println!("deleted agent {project}/{slug}");
            Ok(())
        }
        "role" => {
            let slug = args.get(2).ok_or_else(|| {
                format!(
                    "usage: corpus agent role <project> <slug> [<{}>]",
                    corpus_core::AgentRole::names()
                )
            })?;
            match args.get(3) {
                None => {
                    let config = store.load_agent(project, slug).map_err(|e| e.to_string())?;
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
                Some(raw) => {
                    let role = corpus_core::AgentRole::parse(raw).ok_or_else(|| {
                        format!(
                            "unknown role {raw:?} — one of {}",
                            corpus_core::AgentRole::names()
                        )
                    })?;
                    store
                        .set_agent_role(project, slug, role)
                        .map_err(|e| e.to_string())?;
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
            println!(
                "{:<24} {:<14} {:<10} {}",
                "AGENT", "CURRENT", "INFERRED", "ACTION"
            );
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
        _ => Err(
            "usage: corpus agent list|new|clone|delete|role|migrate-roles <project> [<slug>] ..."
                .to_string(),
        ),
    }
}

/// `corpus mission ...`
pub fn mission_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    let sub = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| "usage: corpus mission list|new|delete <project> ...".to_string())?;
    let project = args.get(1).ok_or("missing project slug")?;

    match sub {
        "list" => {
            for (slug, mission) in store.list_missions(project).map_err(|e| e.to_string())? {
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
        "new" => {
            let slug = args.get(2).ok_or("usage: corpus mission new <project> <slug> --agent <agent> [--budget <val>] [--pin <repo=rev>] <brief>")?;
            let mut agent: Option<String> = None;
            let mut budget: Option<String> = None;
            // Missions stamp the project's effective source selection. A
            // stored project pin overrides the plugin default; --pin below
            // is the final per-mission override.
            let mut pins: std::collections::BTreeMap<String, String> =
                corpus_core::plugin_sources(&store, project)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|source| {
                        let rev = source.default_rev().to_string();
                        (source.name, rev)
                    })
                    .collect();
            pins.extend(
                corpus_core::Project::load(&store, project)
                    .map_err(|e| e.to_string())?
                    .pins,
            );
            let mut brief_words: Vec<String> = Vec::new();
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--agent" => {
                        agent = Some(
                            args.get(i + 1)
                                .ok_or("missing value after --agent")?
                                .clone(),
                        );
                        i += 2;
                    }
                    "--budget" => {
                        budget = Some(
                            args.get(i + 1)
                                .ok_or("missing value after --budget")?
                                .clone(),
                        );
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
            // Reject a rev that could never resolve, here at authoring —
            // not at launch (structural, no network).
            for (repo, rev) in &pins {
                corpus_core::validate_pin(&store, project, repo, rev).map_err(|e| e.to_string())?;
            }
            let mission = Mission {
                agent,
                pins,
                budget,
                created: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                name: None,
                session: None,
                control: None,
                opencode_session: None,
                environment_session: None,
                launch_requested: None,
                delete_requested: None,
                dispatch: None,
            };
            store
                .write_mission(project, slug, &mission, &brief_words.join(" "))
                .map_err(|e| e.to_string())?;
            println!("created mission {project}/{slug}");
            Ok(())
        }
        "delete" => {
            let slug = args
                .get(2)
                .ok_or("usage: corpus mission delete <project> <slug>")?;
            if store.ensure_mission_deletable(project, slug).is_ok() {
                store.delete_mission(project, slug).map_err(|e| e.to_string())?;
                println!("deleted mission {project}/{slug}");
                return Ok(());
            }
            let mut mission = store
                .load_mission(project, slug)
                .map_err(|e| e.to_string())?;
            mission.launch_requested = None;
            mission.delete_requested.get_or_insert(MissionDeleteRequest {
                requested_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            });
            store
                .update_mission(project, slug, &mission)
                .map_err(|e| e.to_string())?;
            println!(
                "deletion requested for mission {project}/{slug}; open corpus-app to complete lifecycle teardown"
            );
            Ok(())
        }
        _ => Err("usage: corpus mission list|new|delete <project> [<slug>] ...".to_string()),
    }
}

/// `corpus finding list|show ...` — a thin CLI over the same tolerant core
/// projection used by MCP and, later, the desktop app.
pub fn finding_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    match args.first().map(String::as_str) {
        Some("list") => {
            let project = args.get(1).ok_or(
                "usage: corpus finding list <project> [--severity <level>] [--exclude-unrated] [--text <query>] [--sort newest|severity] [--limit N]",
            )?;
            let query = parse_finding_query(&args[2..])?;
            let cards = corpus_core::finding_cards(&store, project).map_err(|e| e.to_string())?;
            let cards = corpus_core::query_findings(&cards, &query);
            if cards.is_empty() {
                println!("(no matching findings) {project}");
                return Ok(());
            }
            println!("SEVERITY\tTIMESTAMP\tREFERENCE\tTITLE\tPATH\tWARNINGS");
            for card in cards {
                let severity = card
                    .severity
                    .map(|value| value.as_str().to_ascii_uppercase())
                    .unwrap_or_else(|| "UNRATED".to_string());
                let timestamp = card
                    .timestamp
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let warnings = card
                    .warnings
                    .iter()
                    .map(|warning| warning.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{severity}\t{timestamp}\t{}\t{}\t{}\t{warnings}",
                    card.reference,
                    card.title.replace('\t', " ").replace('\n', " "),
                    card.path.display(),
                );
            }
            Ok(())
        }
        Some("show") => {
            let project = args
                .get(1)
                .ok_or("usage: corpus finding show <project> <findings/path.md>")?;
            let path = args
                .get(2)
                .ok_or("usage: corpus finding show <project> <findings/path.md>")?;
            if args.len() != 3 {
                return Err("usage: corpus finding show <project> <findings/path.md>".into());
            }
            let body = corpus_core::read_finding(&store, project, path)
                .map_err(|error| error.to_string())?;
            print!("{body}");
            Ok(())
        }
        _ => Err(
            "usage: corpus finding list <project> [filters] | show <project> <findings/path.md>"
                .into(),
        ),
    }
}

fn parse_finding_query(args: &[String]) -> Result<FindingQuery, String> {
    let mut query = FindingQuery::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--severity" => {
                let raw = args.get(i + 1).ok_or("missing value after --severity")?;
                for value in raw.split(',') {
                    query.severities.insert(FindingSeverity::parse(value).ok_or_else(|| {
                        format!(
                            "invalid finding severity {value:?}; expected critical, high, medium, or low"
                        )
                    })?);
                }
                i += 2;
            }
            "--exclude-unrated" => {
                query.include_unrated = false;
                i += 1;
            }
            "--text" => {
                query.text = Some(args.get(i + 1).ok_or("missing value after --text")?.clone());
                i += 2;
            }
            "--sort" => {
                query.sort = match args.get(i + 1).map(String::as_str) {
                    Some("newest") => FindingSort::Newest,
                    Some("severity") => FindingSort::Severity,
                    Some(value) => {
                        return Err(format!(
                            "invalid finding sort {value:?}; expected newest or severity"
                        ))
                    }
                    None => return Err("missing value after --sort".into()),
                };
                i += 2;
            }
            "--limit" => {
                let value: usize = args
                    .get(i + 1)
                    .ok_or("missing value after --limit")?
                    .parse()
                    .map_err(|error| format!("--limit: {error}"))?;
                if value == 0 {
                    return Err("--limit must be positive".into());
                }
                query.limit = Some(value);
                i += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(query)
}

#[cfg(test)]
mod finding_query_tests {
    use super::*;

    #[test]
    fn parses_repeatable_and_comma_separated_filters() {
        let args = [
            "--severity",
            "critical,high",
            "--severity",
            "medium",
            "--exclude-unrated",
            "--sort",
            "severity",
            "--limit",
            "5",
            "--text",
            "mint",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let query = parse_finding_query(&args).unwrap();
        assert_eq!(
            query.severities,
            std::collections::BTreeSet::from([
                FindingSeverity::Critical,
                FindingSeverity::High,
                FindingSeverity::Medium,
            ])
        );
        assert!(!query.include_unrated);
        assert_eq!(query.sort, FindingSort::Severity);
        assert_eq!(query.limit, Some(5));
        assert_eq!(query.text.as_deref(), Some("mint"));
    }

    #[test]
    fn refuses_unknown_filter_values() {
        for args in [
            vec!["--severity".to_string(), "urgent".to_string()],
            vec!["--sort".to_string(), "risk".to_string()],
            vec!["--limit".to_string(), "0".to_string()],
        ] {
            assert!(parse_finding_query(&args).is_err());
        }
    }
}

/// `corpus audit <project> [--tail N]`
///
/// The operator's window onto what a curator did. Deliberately read-only
/// and deliberately not an MCP tool: the subject of a log should not be
/// its reader, so no agent has a way to see — or edit — this.
pub fn audit_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    let project = args
        .first()
        .ok_or("usage: corpus audit <project> [--tail N]")?;
    let mut tail = 50usize;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tail" => {
                tail = args
                    .get(i + 1)
                    .ok_or("missing value after --tail")?
                    .parse()
                    .map_err(|e| format!("--tail: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let records = corpus_core::audit::tail(&store, project, tail).map_err(|e| e.to_string())?;
    if records.is_empty() {
        println!(
            "no recorded changes for {project} ({})",
            corpus_core::audit::log_path(&store, project).display()
        );
        return Ok(());
    }
    for record in records {
        println!(
            "{}  {:<9} {:<22} {:<28} {}",
            record.ts,
            format!("{:?}", record.outcome).to_lowercase(),
            record.actor,
            record.op,
            record.target
        );
        if !record.detail.trim().is_empty() {
            for line in record.detail.lines().take(3) {
                println!("             {line}");
            }
        }
    }
    Ok(())
}

/// `corpus refusals <project> [--tail N] [--gate G]`
///
/// What the server turned away, and which gate did it. The companion to
/// `corpus audit`: that one records the acts a curator completed, this one
/// the calls nobody completed at all.
///
/// Read it BEFORE the transcript. A run that misbehaved and shows no
/// refusals here was not stopped by the corpus server — that narrows the
/// hunt to opencode's own permission block, or to a tool description
/// pointing somewhere the agent cannot reach.
///
/// Operator-only and read-only, like `audit`: an agent that could read this
/// would be reading a map of every gate and the exact wording that trips
/// it.
pub fn refusals_cmd(args: &[String]) -> Result<(), String> {
    use corpus_core::refusal;
    let store = Store::from_env();
    let project = args
        .first()
        .ok_or("usage: corpus refusals <project> [--tail N] [--gate G]")?;
    let mut tail = 50usize;
    let mut gate: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tail" => {
                tail = args
                    .get(i + 1)
                    .ok_or("missing value after --tail")?
                    .parse()
                    .map_err(|e| format!("--tail: {e}"))?;
                i += 2;
            }
            "--gate" => {
                gate = Some(args.get(i + 1).ok_or("missing value after --gate")?.clone());
                i += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let records = refusal::tail(&store, project, tail).map_err(|e| e.to_string())?;
    let records: Vec<_> = match &gate {
        Some(want) => records
            .into_iter()
            .filter(|r| r.gate.as_str() == want.as_str())
            .collect(),
        None => records,
    };
    if records.is_empty() {
        // Said positively: for this log, empty is a finding rather than an
        // absence of data. It is the answer to "was it us?", and the answer
        // is no.
        println!(
            "no refusals recorded for {project}{} ({})",
            gate.as_ref()
                .map(|g| format!(" at gate {g}"))
                .unwrap_or_default(),
            refusal::log_path(&store, project).display()
        );
        println!("nothing the corpus server refused — a run that still misbehaved was stopped somewhere else.");
        return Ok(());
    }
    for record in records {
        println!(
            "{}  {:<9} {:<12} {:<24} {}{}",
            record.ts,
            record.gate.as_str(),
            record.role.as_deref().unwrap_or("-"),
            record.tool,
            record.actor,
            record
                .run_log
                .as_deref()
                .map(|r| format!("  run={r}"))
                .unwrap_or_default()
        );
        for line in record.detail.lines().take(3) {
            println!("             {line}");
        }
        if !record.args.trim().is_empty() && record.args != "{}" {
            println!("             args: {}", record.args);
        }
    }
    Ok(())
}
