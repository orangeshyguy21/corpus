//! Read-only corpus size and category projections.

use std::collections::BTreeMap;
use std::fs;

use crate::error::{Error, Result};
use crate::run_records::RUNS;
use crate::store::{Store, CATEGORIES, LEGACY_ATTACKS, PROBES};

/// File and byte totals for one project's corpus.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CorpusStats {
    pub files: u64,
    pub bytes: u64,
    /// Non-empty knowledge categories in stable category order, followed by
    /// any uncategorized bucket.
    pub categories: Vec<CategoryStat>,
    /// Mission transcript files, deliberately separated from knowledge.
    pub logs: CategoryStat,
}

impl CorpusStats {
    pub fn knowledge_files(&self) -> u64 {
        self.files.saturating_sub(self.logs.files)
    }

    pub fn knowledge_bytes(&self) -> u64 {
        self.bytes.saturating_sub(self.logs.bytes)
    }
}

/// One corpus category's share of the summary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CategoryStat {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
}

/// Walk one project's corpus, counting regular files without following
/// symlinks. Missing corpus directories project as an empty corpus.
pub fn corpus_stats(store: &Store, project: &str) -> Result<CorpusStats> {
    let root = store.project_corpus_dir(project);
    let mut stats = CorpusStats::default();
    let mut by_name: BTreeMap<String, CategoryStat> = CATEGORIES
        .iter()
        .map(|category| {
            (
                category.to_string(),
                CategoryStat {
                    name: category.to_string(),
                    files: 0,
                    bytes: 0,
                },
            )
        })
        .collect();

    match root.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let mut stack = vec![root.clone()];
            while let Some(directory) = stack.pop() {
                for entry in fs::read_dir(&directory)? {
                    let entry = entry?;
                    let path = entry.path();
                    let Ok(kind) = entry.file_type() else {
                        continue;
                    };
                    if kind.is_dir() {
                        stack.push(path);
                    } else if kind.is_file() {
                        let Ok(metadata) = entry.metadata() else {
                            continue;
                        };
                        stats.files += 1;
                        stats.bytes += metadata.len();
                        let category = path
                            .strip_prefix(&root)
                            .ok()
                            .filter(|relative| relative.components().count() > 1)
                            .and_then(|relative| relative.components().next())
                            .and_then(|component| component.as_os_str().to_str())
                            .unwrap_or("other");
                        // Legacy artifacts remain visible as probes during the
                        // compatibility window instead of becoming an
                        // uncategorized or duplicate UI bucket.
                        let category = if category == LEGACY_ATTACKS {
                            PROBES
                        } else {
                            category
                        };
                        let slot =
                            by_name
                                .entry(category.to_string())
                                .or_insert_with(|| CategoryStat {
                                    name: category.to_string(),
                                    files: 0,
                                    bytes: 0,
                                });
                        slot.files += 1;
                        slot.bytes += metadata.len();
                    }
                }
            }
        }
        Ok(_) => {
            return Err(Error::Store(format!(
                "corpus root {} is not a real directory",
                root.display()
            )));
        }
    }

    stats.logs = by_name.remove(RUNS).unwrap_or_else(|| CategoryStat {
        name: RUNS.to_string(),
        ..CategoryStat::default()
    });
    let mut categories: Vec<CategoryStat> = CATEGORIES
        .iter()
        .filter(|category| **category != RUNS)
        .map(|category| by_name.remove(*category).expect("seeded above"))
        .collect();
    categories.extend(by_name.into_values());
    categories.retain(|category| category.files > 0);
    stats.categories = categories;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::run_records::mission_logs;

    fn tmp_store(tag: &str) -> Store {
        let world = std::env::temp_dir().join(format!("corpus-stats-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn empty_stats() -> CorpusStats {
        CorpusStats {
            logs: CategoryStat {
                name: RUNS.to_string(),
                files: 0,
                bytes: 0,
            },
            ..CorpusStats::default()
        }
    }

    #[test]
    fn corpus_stats_counts_files_and_bytes() {
        let store = tmp_store("categories");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        assert_eq!(corpus_stats(&store, "p").unwrap(), empty_stats());
        let corpus = store.project_corpus_dir("p");
        write(&corpus.join("findings/1.md"), "hello world\n");
        write(&corpus.join("techniques/quote.md"), "abcd");
        write(&corpus.join("probes/probe-a/probe.md"), "body bytes\n");
        write(&corpus.join("probes/probe-a/run.sh"), "#!/bin/sh\n");

        let stats = corpus_stats(&store, "p").unwrap();
        assert_eq!(stats.files, 4);
        assert_eq!(stats.bytes, 37);
        let names: Vec<&str> = stats
            .categories
            .iter()
            .map(|category| category.name.as_str())
            .collect();
        assert_eq!(names, ["techniques", "findings", "probes"]);
        assert_eq!(stats.categories[0].files, 1);
        assert_eq!(stats.categories[1].bytes, 12);
        assert_eq!(stats.categories[2].files, 2);
        assert_eq!(corpus_stats(&store, "ghost").unwrap(), empty_stats());
    }

    #[test]
    fn legacy_attacks_are_projected_as_probes() {
        let store = tmp_store("legacy-probes");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let corpus = store.project_corpus_dir("p");
        write(&corpus.join("attacks/replay/attack.md"), "legacy\n");
        write(&corpus.join("attacks/replay/run.sh"), "run\n");

        let stats = corpus_stats(&store, "p").unwrap();
        assert_eq!(stats.categories.len(), 1);
        assert_eq!(stats.categories[0].name, PROBES);
        assert_eq!(stats.categories[0].files, 2);
    }

    #[test]
    fn mission_logs_are_split_out_of_the_categories() {
        let store = tmp_store("logs");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let corpus = store.project_corpus_dir("p");
        write(&corpus.join("findings/1.md"), "hello world\n");
        write(&corpus.join("runs/1786891368-verify.raw"), "transcript\n");
        write(&corpus.join("runs/1786856299-discover.json"), "{}");
        write(&corpus.join("triage-report.md"), "note\n");

        let stats = corpus_stats(&store, "p").unwrap();
        assert_eq!((stats.files, stats.bytes), (4, 30));
        assert_eq!((stats.knowledge_files(), stats.knowledge_bytes()), (2, 17));
        let names: Vec<&str> = stats
            .categories
            .iter()
            .map(|category| category.name.as_str())
            .collect();
        assert_eq!(names, ["findings", "other"]);
        assert_eq!((stats.logs.files, stats.logs.bytes), (2, 13));

        let logs = mission_logs(&store, "p").unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].name, "1786891368-verify.raw");
        assert_eq!(logs[0].agent.as_deref(), Some("verify"));
        assert_eq!(logs[0].started, 1_786_891_368);
        assert_eq!(logs[0].kind, "raw");
        assert_eq!(logs[0].bytes, 11);
        assert_eq!(logs[1].agent.as_deref(), Some("discover"));
        assert!(mission_logs(&store, "ghost").unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn corpus_stats_never_follows_symlinks() {
        let store = tmp_store("symlink");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let corpus = store.project_corpus_dir("p");
        let outside = store.root().join("outside-corpus");
        write(&outside.join("secret.md"), "secret");

        std::os::unix::fs::symlink(&outside, corpus.join("linked")).unwrap();
        assert_eq!(
            corpus_stats(&store, "p").unwrap(),
            empty_stats(),
            "a nested directory symlink is not traversed"
        );

        fs::remove_file(corpus.join("linked")).unwrap();
        fs::remove_dir_all(&corpus).unwrap();
        std::os::unix::fs::symlink(&outside, &corpus).unwrap();
        let error = corpus_stats(&store, "p").unwrap_err();
        assert!(
            error.to_string().contains("not a real directory"),
            "{error}"
        );
    }
}
