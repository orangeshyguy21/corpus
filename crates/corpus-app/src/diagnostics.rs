//! Executable-level delivery for Corpus's structured operational events.

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

const LOG_DIRECTORY: &str = "var/diagnostics";
const LOG_PREFIX: &str = "corpus-app";
const LOG_SUFFIX: &str = "jsonl";
const RETAINED_LOG_FILES: usize = 8;

/// Keeps the non-blocking writer alive and flushes queued events when dropped.
pub struct DiagnosticsGuard {
    _writer: WorkerGuard,
}

/// Install the process-wide subscriber before application state or chat starts.
///
/// Only Corpus operational events enter this sink. The rolling appender
/// retains at most eight daily JSONL files under the operator-owned data root.
pub fn install_local_subscriber() -> Result<DiagnosticsGuard, String> {
    install_subscriber_at(corpus_core::data_root().join(LOG_DIRECTORY))
}

fn install_subscriber_at(directory: PathBuf) -> Result<DiagnosticsGuard, String> {
    let appender = build_appender(&directory)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("corpus.lifecycle=info,corpus.delivery=info"))
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .with_ansi(false)
        .with_writer(writer)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|error| format!("cannot install tracing subscriber: {error}"))?;
    Ok(DiagnosticsGuard { _writer: guard })
}

fn build_appender(directory: &Path) -> Result<RollingFileAppender, String> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix(LOG_SUFFIX)
        .max_log_files(RETAINED_LOG_FILES)
        .build(directory)
        .map_err(|error| {
            format!(
                "cannot open diagnostic log directory {}: {error}",
                directory.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "corpus-app-diagnostics-{label}-{}-{}",
            std::process::id(),
            corpus_core::fnv1a_hex(format!("{:?}", std::time::Instant::now()).as_bytes())
        ))
    }

    #[test]
    fn rolling_sink_prunes_matching_files_but_preserves_unrelated_entries() {
        let directory = temp_path("retention");
        std::fs::create_dir_all(&directory).unwrap();
        for day in 1..=12 {
            std::fs::write(
                directory.join(format!("{LOG_PREFIX}.2026-07-{day:02}.{LOG_SUFFIX}")),
                b"old\n",
            )
            .unwrap();
        }
        std::fs::write(directory.join("operator-note.txt"), b"keep").unwrap();

        let appender = build_appender(&directory).unwrap();
        drop(appender);

        let retained = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(LOG_PREFIX) && name.ends_with(LOG_SUFFIX)
            })
            .count();
        assert!(retained <= RETAINED_LOG_FILES);
        assert!(directory.join("operator-note.txt").is_file());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unavailable_sink_returns_an_error_instead_of_panicking() {
        let root = temp_path("unavailable");
        std::fs::create_dir_all(&root).unwrap();
        let blocked = root.join("not-a-directory");
        std::fs::write(&blocked, b"file").unwrap();

        let error = build_appender(&blocked).unwrap_err();
        assert!(error.contains("cannot open diagnostic log directory"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
