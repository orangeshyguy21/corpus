//! corpus: vulnerability research platform command-line entry point.
//!
//! The desktop operator UI lives in `corpus-app`; this binary keeps the
//! headless scripting and diagnostic commands.

// A small headless pin regression remains beside the launch adapter below.
#![allow(clippy::items_after_test_module)]

mod cli;
mod commands;

use std::io::Write;
use std::process::ExitCode;

use corpus_core::{Scope, Store};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        println!("{}", cli::usage());
        return ExitCode::SUCCESS;
    }
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(error) => return report_parse_error(error),
    };
    let result = match command {
        cli::CliCommand::Run(args) => run_cmd(args),
        cli::CliCommand::Plugin(args) => commands::plugin::run(args.command),
        cli::CliCommand::Models(args) => commands::models::run(args.command),
        cli::CliCommand::Project(args) => commands::project::run(args.command),
        cli::CliCommand::Agent(args) => commands::agent::run(args.command),
        cli::CliCommand::Mission(args) => commands::mission::run(args.command),
        cli::CliCommand::Finding(args) => commands::finding::run(args.command),
        cli::CliCommand::Audit(args) => commands::operator_logs::audit(args),
        cli::CliCommand::Refusals(args) => commands::operator_logs::refusals(args),
        cli::CliCommand::Unknown(args) => Err(format!(
            "unknown command: {}\n\n{}",
            args.first().map(String::as_str).unwrap_or(""),
            cli::usage()
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("corpus: error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn report_parse_error(error: clap::Error) -> ExitCode {
    use clap::error::ErrorKind;

    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        print!("{error}");
        return ExitCode::SUCCESS;
    }
    let rendered = error.to_string();
    let rendered = rendered.strip_prefix("error: ").unwrap_or(&rendered);
    eprint!("corpus: error: {rendered}");
    ExitCode::FAILURE
}

/// `corpus run <agent> [-m model] [--research] <mission...>` — run an
/// opencode mission with the transcript logged to the project corpus
/// `runs/` automatically. The agent is materialized to `.opencode/agent/`
/// first (bare names), then opencode spawns with CORPUS_PROJECT set,
/// so the MCP server writes into the project scope.
fn run_cmd(args: cli::RunArgs) -> Result<(), String> {
    let cli::RunArgs {
        agent,
        model,
        research,
        mission,
    } = args;
    let mission = mission.join(" ");

    let store = Store::from_env();
    // No default project: a run that cannot name its project would write a
    // whole mission's output into someone else's corpus.
    let scope = Scope::from_env_strict(&store)?;

    // Check the agent exists on the project.
    if store.load_agent(&scope.project, &agent).is_err() {
        let agents = store
            .list_agents(&scope.project)
            .map_err(|e| e.to_string())?;
        let have: Vec<String> = agents.iter().map(|(s, _)| s.clone()).collect();
        return Err(format!(
            "project {} has no agent named {:?} (agents: {})",
            scope.project,
            agent,
            have.join(", ")
        ));
    }

    // Materialize the project's agents (clear + render to .opencode/agent/):
    // the agent list opencode shows is scoped to the project.
    let written = store
        .render_project_agents(&scope.project)
        .map_err(|e| e.to_string())?;
    for path in &written {
        println!("materialized {}", path.display());
    }

    let project_record =
        corpus_core::Project::load(&store, &scope.project).map_err(|error| error.to_string())?;
    // The app stamps every visible source selection into a mission. The
    // headless command has no mission record, so fill only missing entries
    // from the plugin manifest defaults; an empty project pin map must not
    // open a v1 session with no source trees.
    let sources =
        corpus_core::plugin_sources(&store, &scope.project).map_err(|error| error.to_string())?;
    let pins = effective_headless_pins(project_record.pins.clone(), sources);
    let resolved = corpus_core::prepare_source_pins(&store, &scope.project, &pins)
        .map_err(|error| error.to_string())?;
    let pins_json = (!resolved.is_empty())
        .then(|| serde_json::to_string(&resolved))
        .transpose()
        .map_err(|error| error.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let environment_id = corpus_core::EnvironmentSessionId {
        project: scope.project.clone(),
        mission: format!("headless-{stamp}"),
        generation: stamp,
    };
    let mut environment = corpus_core::open_environment_session(&store, environment_id, resolved)
        .map_err(|error| error.to_string())?;
    let environment_key = environment.as_ref().map(|record| record.id.storage_key());
    let run_result = (|| {
        let mut session = corpus_core::RunSession::spawn_headless_with_environment(
            &scope.project,
            &agent,
            model.as_deref(),
            &mission,
            pins_json.as_deref(),
            environment_key.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        println!("logging to {}", session.transcript.display());
        let status = drain_session(&mut session)?;
        if !status.success() {
            return Err(format!("opencode exited with {status}"));
        }

        if research {
            let prompt = format!(
                "Read the operator run transcript at {}. Curate and distill it \
             into the corpus: (1) technique card(s) under the project corpus \
             techniques/ for every attack surface analyzed or attempted — \
             mechanics, preconditions, the oracle that would catch it, and \
             status (fired / analyzed-only / unresolved-lead) — citing this \
             run log. (2) A 'failure modes' section per card citing transcript \
             moments. (3) An honest run outcome at the end of each card: \
             completed / truncated / blocked, and why. (4) If the transcript \
             yields a fresh lead worth attacking, write one hypothesis entry \
             under the project corpus hypotheses/ (target surface, rationale, \
             suggested mission text, source citations). Remember the \
             contamination rule: never read benchmarks/** or plugins/**.",
                session.transcript.display()
            );
            println!("researching {} ...", session.transcript.display());
            let mut log = std::fs::OpenOptions::new()
                .append(true)
                .open(&session.transcript)
                .map_err(|e| e.to_string())?;
            log.write_all(b"\n\n# --- researcher pass ---\n\n")
                .map_err(|e| e.to_string())?;
            drop(log);
            // Materialize the researcher agent for the follow-up.
            let _ = store
                .render_agent(&scope.project, "researcher")
                .map_err(|e| e.to_string())?;
            let mut session = corpus_core::RunSession::spawn_headless_append_with_environment(
                &scope.project,
                "researcher",
                model.as_deref(),
                &prompt,
                &session.transcript,
                pins_json.as_deref(),
                environment_key.as_deref(),
            )
            .map_err(|e| e.to_string())?;
            let status = drain_session(&mut session)?;
            if !status.success() {
                return Err(format!("researcher exited with {status}"));
            }
        }
        Ok(())
    })();
    let close_result = environment.as_mut().map(|environment| {
        corpus_core::close_environment_session(&store, environment)
            .map_err(|error| error.to_string())
    });
    match (run_result, close_result) {
        (Ok(()), None | Some(Ok(()))) => Ok(()),
        (Ok(()), Some(Err(cleanup))) => Err(cleanup),
        (Err(run), None | Some(Ok(()))) => Err(run),
        (Err(run), Some(Err(cleanup))) => {
            Err(format!("{run}; environment cleanup also failed: {cleanup}"))
        }
    }
}

fn effective_headless_pins(
    mut selected: std::collections::BTreeMap<String, String>,
    sources: Vec<corpus_core::SourceRevs>,
) -> std::collections::BTreeMap<String, String> {
    for source in sources {
        selected.entry(source.name).or_insert(source.pinned);
    }
    selected
}

#[cfg(test)]
mod headless_pin_tests {
    use std::collections::BTreeMap;

    use super::effective_headless_pins;

    #[test]
    fn manifest_defaults_do_not_overwrite_project_pins() {
        let selected = BTreeMap::from([("nutshell".into(), "custom".into())]);
        let sources = vec![
            corpus_core::SourceRevs {
                name: "nutshell".into(),
                pinned: "0.20.3".into(),
                revs: vec!["main".into(), "0.20.3".into()],
                refs_fetched: None,
            },
            corpus_core::SourceRevs {
                name: "nuts".into(),
                pinned: "main".into(),
                revs: vec!["main".into()],
                refs_fetched: None,
            },
        ];
        assert_eq!(
            effective_headless_pins(selected, sources),
            BTreeMap::from([
                ("nutshell".into(), "custom".into()),
                ("nuts".into(), "main".into()),
            ])
        );
    }
}

/// Pump a session's lines to the terminal until it exits, then flush
/// whatever the pumps still held.
fn drain_session(
    session: &mut corpus_core::RunSession,
) -> Result<std::process::ExitStatus, String> {
    loop {
        while let Some(line) = session.poll_line() {
            if line.stderr {
                eprintln!("{}", line.text);
            } else {
                println!("{}", line.text);
            }
        }
        if let Some(status) = session.try_exit() {
            while let Some(line) = session.poll_line_timeout(std::time::Duration::from_millis(400))
            {
                if line.stderr {
                    eprintln!("{}", line.text);
                } else {
                    println!("{}", line.text);
                }
            }
            return Ok(status);
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}
