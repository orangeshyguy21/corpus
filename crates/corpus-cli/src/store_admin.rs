//! `corpus project/team/template/promote/store` admin commands: the scoped
//! store (core templates, projects, teams, corpora) exposed headlessly.
//! Builds directly on corpus-core's store module — the deck will ride the
//! same API later.

use std::collections::BTreeMap;

use corpus_core::{
    core_agent_instances, AgentInstance, Store, TeamSpec, Templates,
};

/// Parse `name=template[?model=model]` agent instantiations. Starts EMPTY —
/// the caller decides whether the core defaults come in. This is deliberate:
/// `team edit` must never re-seed agents the operator dropped, so the seed
/// must not live inside the parser.
fn parse_agent_instances(raw: &[&str]) -> Result<BTreeMap<String, AgentInstance>, String> {
    let mut agents = BTreeMap::new();
    for spec in raw {
        let (name, rest) = spec.split_once('=').ok_or_else(|| {
            format!("bad --agent {spec:?}: expected name=template[?model=model]")
        })?;
        if name.is_empty() {
            return Err(format!("--agent has an empty name: {spec:?}"));
        }
        let (template, model) = match rest.split_once('?') {
            Some((template, opts)) => {
                let model = opts.strip_prefix("model=").map(str::to_string).ok_or_else(|| {
                    format!("bad --agent {spec:?}: only ?model= is supported")
                })?;
                (template.to_string(), Some(model))
            }
            None => (rest.to_string(), None),
        };
        agents.insert(
            name.to_string(),
            AgentInstance {
                template,
                model,
            },
        );
    }
    Ok(agents)
}

/// The agent set for `team new`: the core pair by default, EXACTLY the
/// explicitly-passed set when `--agent` flags are given (no silent seeding).
fn agents_for_new(opts: &TeamOptions) -> Result<BTreeMap<String, AgentInstance>, String> {
    if opts.agents.is_empty() {
        Ok(core_agent_instances())
    } else {
        parse_agent_instances(&opts.agents.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

/// Apply `team edit` mutations to a loaded spec. Only ever touches what the
/// flags name: `--agent` inserts/replaces exactly those agent names,
/// `--drop-agent` removes exactly those; nothing else is re-seeded.
fn apply_team_edits(spec: &mut TeamSpec, opts: &TeamOptions) -> Result<(), String> {
    if let Some(name) = &opts.name {
        spec.name = name.clone();
    }
    if let Some(rev) = &opts.rev {
        if rev == "-" {
            spec.rev_override = None;
        } else {
            spec.rev_override = Some(rev.clone());
        }
    }
    if let Some(budget) = &opts.budget {
        if budget == "-" {
            spec.budget = None;
        } else {
            spec.budget = Some(budget.clone());
        }
    }
    let parsed = parse_agent_instances(&opts.agents.iter().map(String::as_str).collect::<Vec<_>>());
    for (name, instance) in parsed? {
        spec.agents.insert(name, instance);
    }
    for name in &opts.drop_agents {
        spec.agents.remove(name);
    }
    Ok(())
}

#[derive(Debug, Default)]
struct TeamOptions {
    name: Option<String>,
    rev: Option<String>,
    budget: Option<String>,
    agents: Vec<String>,
    drop_agents: Vec<String>,
}

fn parse_team_options(args: &[String]) -> Result<TeamOptions, String> {
    let mut opts = TeamOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                opts.name = Some(args.get(i + 1).ok_or("missing value after --name")?.clone());
                i += 2;
            }
            "--rev" => {
                opts.rev = Some(args.get(i + 1).ok_or("missing value after --rev")?.clone());
                i += 2;
            }
            "--budget" => {
                opts.budget = Some(args.get(i + 1).ok_or("missing value after --budget")?.clone());
                i += 2;
            }
            "--agent" => {
                opts.agents.push(args.get(i + 1).ok_or("missing value after --agent")?.clone());
                i += 2;
            }
            "--drop-agent" => {
                opts.drop_agents
                    .push(args.get(i + 1).ok_or("missing value after --drop-agent")?.clone());
                i += 2;
            }
            other => return Err(format!("unknown team option: {other}")),
        }
    }
    Ok(opts)
}

/// `corpus project ...`
pub fn project_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    match args.first().map(String::as_str) {
        Some("list") => {
            for (slug, project) in store.list_projects().map_err(|e| e.to_string())? {
                println!(
                    "{:<20} {:<24} plugin={} created={}{}",
                    slug,
                    project.name,
                    project.plugin,
                    project.created,
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
            "usage: corpus project list|new <slug>|clone <slug> --to <new>|delete <slug>|rebind <slug> --plugin <name>"
                .to_string(),
        ),
    }
}

/// `corpus team ...`
pub fn team_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    let sub = args.first().map(String::as_str).ok_or_else(|| {
        "usage: corpus team list|new|edit|clone|delete|wipe <project> ...".to_string()
    })?;
    let project = args.get(1).ok_or("missing project slug")?;
    let team = args.get(2);

    match sub {
        "list" => {
            for (slug, spec) in store.list_teams(project).map_err(|e| e.to_string())? {
                println!(
                    "{:<16} gen={} agents=[{}]{}{}",
                    slug,
                    spec.corpus_generation,
                    spec.agents
                        .iter()
                        .map(|(name, inst)| format!("{name}:{}", inst.template))
                        .collect::<Vec<_>>()
                        .join(", "),
                    spec.rev_override
                        .map(|r| format!(" rev={r}"))
                        .unwrap_or_default(),
                    spec.budget
                        .map(|b| format!(" budget={b}"))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }
        "new" => {
            let team = team.ok_or("usage: corpus team new <project> <slug> [--name <label>] [--rev <sha>] [--budget <value>] [--agent name=template?model=...]")?;
            let rest: Vec<String> = args[3..].to_vec();
            let opts = parse_team_options(&rest)?;
            let agents = agents_for_new(&opts)?;
            let spec = store
                .create_team(
                    project,
                    team,
                    opts.name.as_deref().unwrap_or(team),
                    agents,
                    opts.rev.as_deref(),
                    opts.budget.as_deref(),
                )
                .map_err(|e| e.to_string())?;
            println!(
                "created team {project}/{team} ({} agents, generation {})",
                spec.agents.len(),
                spec.corpus_generation
            );
            Ok(())
        }
        "edit" => {
            let team = team.ok_or("usage: corpus team edit <project> <team> [--name <label>] [--rev <sha|->] [--budget <value|->] [--agent ...] [--drop-agent <name>]")?;
            let rest: Vec<String> = args[3..].to_vec();
            let opts = parse_team_options(&rest)?;
            store
                .update_team(project, team, |spec| {
                    apply_team_edits(spec, &opts).map_err(|e| corpus_core::Error::Store(e))
                })
                .map_err(|e| e.to_string())?;
            println!("updated team {project}/{team}");
            Ok(())
        }
        "clone" => {
            let team = team.ok_or("usage: corpus team clone <project> <team> --to <new-team>")?;
            let to = args
                .iter()
                .position(|a| a == "--to")
                .and_then(|i| args.get(i + 1))
                .ok_or("missing --to <new-team>")?
                .clone();
            let (slug, spec) = store.clone_team(project, team, &to).map_err(|e| e.to_string())?;
            println!("cloned team {project}/{team} -> {slug} (generation {})", spec.corpus_generation);
            Ok(())
        }
        "delete" => {
            let team = team.ok_or("usage: corpus team delete <project> <team>")?;
            store.delete_team(project, team).map_err(|e| e.to_string())?;
            println!("deleted team {project}/{team}");
            Ok(())
        }
        "wipe" => {
            let team = team.ok_or("usage: corpus team wipe <project> <team>")?;
            let spec = store.wipe_team_corpus(project, team).map_err(|e| e.to_string())?;
            println!(
                "wiped team corpus {project}/{team} (generation {})",
                spec.corpus_generation
            );
            Ok(())
        }
        _ => Err(
            "usage: corpus team list|new|edit|clone|delete|wipe <project> [<team>] ...".to_string(),
        ),
    }
}

/// `corpus template ...`
pub fn template_cmd(args: &[String]) -> Result<(), String> {
    let store = Store::from_env();
    match args.first().map(String::as_str) {
        Some("list") => {
            // The current project's templates merged with core, shadowing
            // by slug — the same union the deck's editors render.
            let project = corpus_core::project_slug_env();
            for kind in [
                corpus_core::TemplateKind::Permission,
                corpus_core::TemplateKind::Prompt,
                corpus_core::TemplateKind::Agent,
            ] {
                for slug in store.template_slugs(&project, kind) {
                    let origin = if store
                        .project_templates(&project)
                        .has(kind, &slug)
                    {
                        "project"
                    } else {
                        "core"
                    };
                    let extra = match kind {
                        corpus_core::TemplateKind::Permission | corpus_core::TemplateKind::Prompt => {
                            let description = match kind {
                                corpus_core::TemplateKind::Permission => store
                                    .load_permission(&project, &slug)
                                    .map(|t| t.description),
                                _ => store.load_prompt(&project, &slug).map(|t| t.description),
                            }
                            .unwrap_or_default();
                            if description.is_empty() {
                                String::new()
                            } else {
                                format!(" description={description}")
                            }
                        }
                        corpus_core::TemplateKind::Agent => {
                            match store.load_agent(&project, &slug) {
                                Ok(agent) => format!(
                                    " permission={} prompt={}{}",
                                    agent.permission_ref,
                                    agent.prompt_ref,
                                    agent
                                        .model
                                        .as_deref()
                                        .filter(|m| !m.is_empty())
                                        .map(|m| format!(" model={m}"))
                                        .unwrap_or_default()
                                ),
                                Err(error) => format!(" <error: {error}>"),
                            }
                        }
                    };
                    let (name, mode) = match kind {
                        corpus_core::TemplateKind::Agent => match store.load_agent(&project, &slug)
                        {
                            Ok(agent) => (agent.name, format!(" mode={}", agent.mode)),
                            Err(_) => (slug.clone(), String::new()),
                        },
                        corpus_core::TemplateKind::Permission => (
                            store
                                .load_permission(&project, &slug)
                                .map(|t| t.name)
                                .unwrap_or_else(|_| slug.clone()),
                            String::new(),
                        ),
                        corpus_core::TemplateKind::Prompt => (
                            store
                                .load_prompt(&project, &slug)
                                .map(|t| t.name)
                                .unwrap_or_else(|_| slug.clone()),
                            String::new(),
                        ),
                    };
                    println!(
                        "{:<10} {:<8} {:<24} {:<12} {}{}{}",
                        kind.label(),
                        origin,
                        name,
                        slug,
                        mode,
                        extra,
                        if origin == "project" {
                            if store.core_templates().has(kind, &slug) {
                                " (shadows core)"
                            } else {
                                ""
                            }
                        } else {
                            ""
                        }
                    );
                }
            }
            Ok(())
        }
        Some("render") => {
            let name = args.get(1).ok_or("usage: corpus template render <name> [--to <dir>]")?;
            let mut dest: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--to" => {
                        dest = Some(args.get(i + 1).ok_or("missing value after --to")?.clone());
                        i += 2;
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            let project = corpus_core::project_slug_env();
            // Accept a slug OR the template's frontmatter name (the deck
            // authors by label; the store keys by slug).
            let slug = resolve_agent_slug(&store, &project, name)?;
            let agent = store.load_agent(&project, &slug).map_err(|e| e.to_string())?;
            let local = store.project_templates(&project);
            let core_templates: Templates = store.core_templates();
            let default_dest = format!(".opencode/agent/{slug}.md");
            let out_dir = dest.as_deref().unwrap_or(".opencode/agent");
            let out_path = std::path::Path::new(out_dir).join(format!("{slug}.md"));
            std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
            let model = agent.model.as_deref();
            agent
                .render(&local, &core_templates, model, &out_path)
                .map_err(|e| e.to_string())?;
            let note = if out_path.to_string_lossy() == default_dest {
                ""
            } else {
                " (not the checked-in location)"
            };
            println!("rendered {} -> {}{}", name, out_path.display(), note);
            Ok(())
        }
        Some("delete") => {
            let name = args.get(1).ok_or("usage: corpus template delete <name> [--project <slug>]")?;
            let mut project = corpus_core::project_slug_env();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--project" => {
                        project = args.get(i + 1).ok_or("missing value after --project")?.clone();
                        i += 2;
                    }
                    other => return Err(format!("unknown option: {other}")),
                }
            }
            let slug = resolve_agent_slug(&store, &project, name)?;
            store
                .delete_template(&project, corpus_core::TemplateKind::Agent, &slug)
                .map_err(|e| e.to_string())?;
            println!("deleted {project} agent template {slug}");
            Ok(())
        }
        _ => Err(
            "usage: corpus template list|render <name> [--to <dir>]|delete <name> [--project <slug>]"
                .to_string(),
        ),
    }
}

/// Resolve a template name (slug or frontmatter label) to its slug,
/// project-then-core.
fn resolve_agent_slug(store: &Store, project: &str, name: &str) -> Result<String, String> {
    let local = store.project_templates(project);
    let core = store.core_templates();
    if local.agents.join(format!("{name}.md")).is_file()
        || core.agents.join(format!("{name}.md")).is_file()
    {
        return Ok(name.to_string());
    }
    for slug in store.template_slugs(project, corpus_core::TemplateKind::Agent) {
        if let Ok(template) = store.load_agent(project, &slug) {
            if template.name == name {
                return Ok(slug);
            }
        }
    }
    Err(format!("no agent template {name:?} (by slug or frontmatter name)"))
}

/// `corpus promote <project> <team> <category> <entry> [--confirm]`
pub fn promote_cmd(args: &[String]) -> Result<(), String> {
    let project = args.get(0).ok_or("usage: corpus promote <project> <team> <category> <entry> [--confirm]")?;
    let team = args.get(1).ok_or("usage: corpus promote <project> <team> <category> <entry> [--confirm]")?;
    let category = args.get(2).ok_or("missing category")?;
    let entry = args.get(3).ok_or("missing entry")?;
    let confirm = args.iter().any(|a| a == "--confirm");
    let store = Store::from_env();
    let promoted = store
        .promote_entry(project, team, category, entry, confirm)
        .map_err(|e| e.to_string())?;
    println!(
        "promoted {category}/{entry} -> {} (sensitivity: {}, from: {})",
        promoted.entry.display(),
        promoted.sensitivity.as_str(),
        promoted.provenance
    );
    Ok(())
}

/// `corpus store migrate [--dry-run] [--project <slug>]`
pub fn store_cmd(args: &[String]) -> Result<(), String> {
    let mut project = corpus_core::DEFAULT_PROJECT_SLUG.to_string();
    let mut dry_run = false;
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
            other => return Err(format!("unknown store option: {other}")),
        }
    }
    let store = Store::from_env();
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
    println!("default team: {project}/default (backward-compat unscoped scope)");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn opts_from(args: &[&str]) -> TeamOptions {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_team_options(&v).expect("parse team options")
    }

    #[test]
    fn team_new_with_explicit_agents_is_exactly_that_set() {
        let opts = opts_from(&["--agent", "critic=researcher?model=remote", "--agent", "scout=researcher"]);
        let agents = agents_for_new(&opts).unwrap();
        assert_eq!(agents.len(), 2, "no silent core seeding with --agent");
        assert_eq!(agents["critic"].template, "researcher");
        assert_eq!(agents["critic"].model.as_deref(), Some("remote"));
        assert!(!agents.contains_key("operator"));
        assert!(!agents.contains_key("researcher"));
    }

    #[test]
    fn team_new_without_flags_defaults_to_core_pair() {
        let agents = agents_for_new(&opts_from(&[])).unwrap();
        assert_eq!(agents.len(), 2);
        assert!(agents.contains_key("operator"));
        assert!(agents.contains_key("researcher"));
    }

    #[test]
    fn edit_drop_then_add_does_not_resurrect_dropped_agents() {
        let drop_operator = opts_from(&["--drop-agent", "operator"]);
        let add_critic = opts_from(&["--agent", "critic=researcher"]);
        let mut spec = TeamSpec {
            agents: core_agent_instances(),
            ..Default::default()
        };
        apply_team_edits(&mut spec, &drop_operator).unwrap();
        assert!(!spec.agents.contains_key("operator"));
        apply_team_edits(&mut spec, &add_critic).unwrap();
        assert!(
            !spec.agents.contains_key("operator"),
            "a later --agent must not re-seed the core pair"
        );
        assert!(spec.agents.contains_key("critic"));
        assert!(spec.agents.contains_key("researcher"));
    }
}