//! Finding projection CLI commands.

use corpus_core::{FindingQuery, Store};

use crate::cli::FindingCommand;

pub(crate) fn run(command: FindingCommand) -> Result<(), String> {
    let store = Store::from_env();
    match command {
        FindingCommand::List {
            project,
            severities,
            exclude_unrated,
            text,
            sort,
            limit,
        } => list(
            &store,
            &project,
            FindingQuery {
                severities: severities.into_iter().collect(),
                include_unrated: !exclude_unrated,
                text,
                sort,
                limit,
            },
        ),
        FindingCommand::Show { project, path } => show(&store, &project, &path),
    }
}

fn list(store: &Store, project: &str, query: FindingQuery) -> Result<(), String> {
    let cards = corpus_core::finding_cards(store, project).map_err(|error| error.to_string())?;
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
            card.title.replace(['\t', '\n'], " "),
            card.path.display(),
        );
    }
    Ok(())
}

fn show(store: &Store, project: &str, path: &str) -> Result<(), String> {
    let body =
        corpus_core::read_finding(store, project, path).map_err(|error| error.to_string())?;
    print!("{body}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use corpus_core::{FindingSeverity, FindingSort};

    use super::*;

    #[test]
    fn query_projection_deduplicates_severities_and_maps_unrated_policy() {
        let query = FindingQuery {
            severities: [FindingSeverity::High, FindingSeverity::High]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            include_unrated: false,
            text: Some("mint".into()),
            sort: FindingSort::Severity,
            limit: Some(5),
        };
        assert_eq!(query.severities, BTreeSet::from([FindingSeverity::High]));
        assert!(!query.include_unrated);
        assert_eq!(query.text.as_deref(), Some("mint"));
        assert_eq!(query.sort, FindingSort::Severity);
        assert_eq!(query.limit, Some(5));
    }
}
