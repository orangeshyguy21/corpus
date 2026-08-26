//! Operator-only audit and refusal log readers.

use corpus_core::Store;

use crate::cli::{AuditArgs, RefusalsArgs};

/// The operator's window onto completed or attempted curator mutations.
pub(crate) fn audit(args: AuditArgs) -> Result<(), String> {
    let store = Store::from_env();
    let records = corpus_core::audit::tail(&store, &args.project, args.tail)
        .map_err(|error| error.to_string())?;
    if records.is_empty() {
        println!(
            "no recorded changes for {} ({})",
            args.project,
            corpus_core::audit::log_path(&store, &args.project).display()
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

/// Read server refusals without exposing their diagnostic map to agents.
pub(crate) fn refusals(args: RefusalsArgs) -> Result<(), String> {
    use corpus_core::refusal;

    let store = Store::from_env();
    let records =
        refusal::tail(&store, &args.project, args.tail).map_err(|error| error.to_string())?;
    let records: Vec<_> = match args.gate {
        Some(gate) => records
            .into_iter()
            .filter(|record| record.gate == gate)
            .collect(),
        None => records,
    };
    if records.is_empty() {
        println!(
            "no refusals recorded for {}{} ({})",
            args.project,
            args.gate
                .map(|gate| format!(" at gate {}", gate.as_str()))
                .unwrap_or_default(),
            refusal::log_path(&store, &args.project).display()
        );
        println!(
            "nothing the corpus server refused — a run that still misbehaved was stopped somewhere else."
        );
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
                .map(|run| format!("  run={run}"))
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
