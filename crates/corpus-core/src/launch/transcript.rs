//! Durable transcript artifacts, OpenCode conversation discovery, and export.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(test)]
use super::command::shell_quote;
use super::executables::resolve_opencode;
use super::process::{bounded_command_output, BoundedOutputSpec};
use super::RunLine;
use crate::error::{Error, Result};
use crate::store::Store;
use corpus_store::EnvironmentSessionId;

const EXPORT_LIMIT: usize = 128 * 1024;
const SESSION_LIST_LIMIT: usize = 1024 * 1024;

pub(super) fn artifact_stem(agent: &str, run_id: Option<&EnvironmentSessionId>) -> String {
    let agent = crate::store::slugify(agent);
    match run_id {
        Some(run_id) => format!(
            "{agent}-{}-g{}",
            crate::store::slugify(&run_id.mission),
            run_id.generation
        ),
        None => agent,
    }
}

pub(super) fn create_piped(
    runs: &Path,
    artifact_stem: &str,
    agent: &str,
    model: Option<&str>,
    mission: &str,
) -> Result<PathBuf> {
    fs::create_dir_all(runs)?;
    let timestamp = now_secs();
    let mission_slug = mission
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let path = runs.join(format!("{timestamp}-{artifact_stem}-{mission_slug}.log"));
    let header = format!(
        "# corpus run\n# agent: {agent}\n# model: {}\n# started: {timestamp}\n# mission: {mission}\n\n",
        model.unwrap_or("(default)")
    );
    let mut file = fs::File::create(&path)?;
    file.write_all(header.as_bytes())?;
    Ok(path)
}

/// Find the oldest unclaimed OpenCode conversation created in this run's
/// working directory after its launch stamp.
pub(super) fn find_opencode_session(
    cwd: &Path,
    launched_at_ms: u64,
    claimed: &BTreeSet<String>,
) -> Result<Option<String>> {
    let opencode = resolve_opencode()?;
    let mut command = Command::new(&opencode);
    command
        .args(["session", "list", "--format", "json", "-n", "50"])
        .current_dir(cwd);
    let output = bounded_command_output(
        command,
        BoundedOutputSpec::new(
            "opencode session list",
            Duration::from_secs(10),
            SESSION_LIST_LIMIT,
        ),
    )?;
    if !output.status.success() {
        return Err(Error::Store(
            "opencode session list reported an error".into(),
        ));
    }
    if output.stdout_truncated {
        return Err(Error::Store(
            "opencode session list exceeded its 1 MiB output cap".into(),
        ));
    }
    let list: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::Store(format!("opencode session list gave bad JSON: {error}")))?;
    Ok(select_session(&list, cwd, launched_at_ms, claimed))
}

fn select_session(
    list: &serde_json::Value,
    cwd: &Path,
    launched_at_ms: u64,
    claimed: &BTreeSet<String>,
) -> Option<String> {
    let entries = list.as_array()?;
    let directory = cwd.to_string_lossy();
    entries
        .iter()
        .filter_map(|entry| {
            let in_directory = entry
                .get("directory")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| value == directory);
            let created = entry.get("created")?.as_u64()?;
            let id = entry.get("id")?.as_str()?;
            (in_directory && created >= launched_at_ms && !claimed.contains(id))
                .then(|| (created, id.to_string()))
        })
        .min_by_key(|(created, _)| *created)
        .map(|(_, id)| id)
}

pub(super) fn export_record(repo: &Path, runs: &Path, session_id: &str) -> Result<PathBuf> {
    let path = runs.join(format!("{session_id}.json"));
    let json = match export_opencode_json(repo, session_id) {
        Ok(json) => json,
        Err(_) if valid_existing_export(&path) => return Ok(path),
        Err(error) => return Err(error),
    };
    fs::create_dir_all(runs)?;
    fs::write(&path, json)?;
    Ok(path)
}

fn export_opencode_json(cwd: &Path, session_id: &str) -> Result<String> {
    export_opencode_json_with_timeout(cwd, session_id, Duration::from_secs(10))
}

fn export_opencode_json_with_timeout(
    cwd: &Path,
    session_id: &str,
    command_timeout: Duration,
) -> Result<String> {
    let opencode = resolve_opencode()?;
    let deadline = Instant::now() + command_timeout;
    let mut last_eof = None;
    for delay_ms in [0, 50, 150] {
        if delay_ms != 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Store(format!(
                "opencode export timed out after {}s",
                command_timeout.as_secs_f32()
            )));
        }
        let mut command = Command::new(&opencode);
        command.arg("export").arg(session_id).current_dir(cwd);
        let output = bounded_command_output(
            command,
            BoundedOutputSpec::new("opencode export", remaining, EXPORT_LIMIT + 1),
        )?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            return Err(Error::Store(if detail.is_empty() {
                "opencode export reported an error".into()
            } else {
                format!("opencode export reported an error: {detail}")
            }));
        }
        if output.stdout_truncated {
            return Err(Error::Store(
                "opencode export exceeded its 128 KiB output cap".into(),
            ));
        }
        match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
            Ok(value) => {
                return serde_json::to_string_pretty(&value)
                    .map_err(|error| Error::Store(format!("cannot serialize export: {error}")));
            }
            Err(error) if error.is_eof() => {
                if output.stdout.len() == EXPORT_LIMIT {
                    return Err(Error::Store(
                        "opencode export hit its 128 KiB output cap".into(),
                    ));
                }
                last_eof = Some(error);
            }
            Err(error) => {
                return Err(Error::Store(format!(
                    "opencode export gave bad JSON: {error}"
                )));
            }
        }
    }
    Err(Error::Store(format!(
        "opencode export gave incomplete JSON after 3 attempts: {}",
        last_eof.expect("each attempt ended at EOF")
    )))
}

fn valid_existing_export(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| {
            value
                .get("messages")
                .is_some_and(serde_json::Value::is_array)
        })
}

pub(super) fn tail_line(path: &Path, file_pos: &mut u64, pending: &mut String) -> Option<RunLine> {
    loop {
        if let Some(end) = pending.find(['\n', '\r']) {
            let line = pending[..end].to_string();
            let consumed = if pending[end..].starts_with("\r\n") {
                end + 2
            } else {
                end + 1
            };
            pending.drain(..consumed);
            return Some(RunLine {
                stderr: false,
                text: line,
            });
        }
        let Ok(metadata) = fs::metadata(path) else {
            return None;
        };
        let length = metadata.len();
        if length < *file_pos {
            *file_pos = 0;
            pending.clear();
        }
        if length <= *file_pos {
            return None;
        }
        let mut buffer = vec![0_u8; (length - *file_pos) as usize];
        let Ok(mut file) = fs::File::open(path) else {
            return None;
        };
        if file.seek(SeekFrom::Start(*file_pos)).is_err() || file.read_exact(&mut buffer).is_err() {
            return None;
        }
        *file_pos = length;
        pending.push_str(&String::from_utf8_lossy(&buffer));
        if pending.len() > 16 * 1024 {
            return Some(RunLine {
                stderr: false,
                text: std::mem::take(pending),
            });
        }
    }
}

pub fn session_conversation(
    store: &Store,
    project: &str,
    workspace: &str,
    session: &str,
    claimed: &BTreeSet<String>,
) -> Option<String> {
    let timestamp = session_stamp(session)?;
    let directory = store.run_workspace_dir(project, workspace).ok()?;
    find_opencode_session(&directory, timestamp.saturating_mul(1000), claimed)
        .ok()
        .flatten()
}

fn session_stamp(session: &str) -> Option<u64> {
    let stem = session.strip_prefix("corpus-")?;
    let (run_stem, timestamp) = stem.rsplit_once('-')?;
    if run_stem.is_empty() {
        return None;
    }
    timestamp.parse().ok()
}

pub fn session_raw_log(store: &Store, project: &str, session: &str) -> Option<PathBuf> {
    corpus_observe::session_raw_log(store, project, session)
}

pub fn run_idle_secs(log: &Path) -> Option<u64> {
    corpus_observe::run_idle_secs(log)
}

pub fn export_session(
    project: &str,
    workspace: &str,
    opencode_session_id: &str,
) -> Result<PathBuf> {
    let store = Store::from_env();
    let repo = store.run_workspace_dir(project, workspace)?;
    let runs = store.project_corpus_dir(project).join("runs");
    export_record(&repo, &runs, opencode_session_id)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, unique_temp_path, EnvVarGuard};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn discovery_selects_the_oldest_eligible_unclaimed_session() {
        let cwd = Path::new("/tmp/project");
        let list = serde_json::json!([
            {"id": "before", "created": 99, "directory": "/tmp/project"},
            {"id": "other-dir", "created": 101, "directory": "/tmp/other"},
            {"id": "claimed", "created": 102, "directory": "/tmp/project"},
            {"id": "ours", "created": 103, "directory": "/tmp/project"},
            {"id": "later", "created": 104, "directory": "/tmp/project"}
        ]);
        let claimed = BTreeSet::from(["claimed".to_string()]);
        assert_eq!(
            select_session(&list, cwd, 100, &claimed).as_deref(),
            Some("ours")
        );
    }

    #[test]
    fn export_retries_a_transient_truncated_document() {
        let _guard = env_lock();
        let bin = unique_temp_path("opencode-export-retry");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("opencode");
        let attempts = bin.join("attempts");
        let attempts = shell_quote(&attempts.to_string_lossy());
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\n\
                 count=0\n\
                 if [ -f {attempts} ]; then count=$(sed -n '1p' {attempts}); fi\n\
                 count=$((count + 1))\n\
                 printf '%s' \"$count\" > {attempts}\n\
                 if [ \"$count\" -eq 1 ]; then\n\
                   printf '%s' '{{\"messages\":[\"partial'\n\
                 else\n\
                   printf '%s\\n' '{{\"messages\":[]}}'\n\
                 fi\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path = EnvVarGuard::set("PATH", &path);
        let exported = export_opencode_json(&bin, "ses_retry").unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&exported).unwrap(),
            serde_json::json!({"messages": []})
        );
        assert_eq!(fs::read_to_string(bin.join("attempts")).unwrap(), "2");
        fs::remove_dir_all(bin).unwrap();
    }

    #[test]
    fn valid_existing_export_survives_a_capped_reexport() {
        let _guard = env_lock();
        let bin = unique_temp_path("opencode-export-cap");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("opencode");
        fs::write(
            &fake,
            "#!/bin/sh\nhead -c 131072 /dev/zero | tr '\\000' x\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path = EnvVarGuard::set("PATH", &path);
        let store_dir = unique_temp_path("export-cap-store");
        let store = Store::new(store_dir.clone());
        let _store = EnvVarGuard::set("CORPUS_STORE", &store_dir);
        store
            .create_project("default", "Default", "cdk-regtest")
            .unwrap();
        let workspace = store
            .provision_run_workspace_with_sources("default", None)
            .unwrap();
        let runs = store.project_corpus_dir("default").join("runs");
        fs::create_dir_all(&runs).unwrap();
        let existing = runs.join("ses_large.json");
        fs::write(&existing, r#"{"info":{"id":"ses_large"},"messages":[]}"#).unwrap();
        assert_eq!(
            export_session("default", &workspace.id, "ses_large").unwrap(),
            existing
        );
        assert!(valid_existing_export(&existing));
        fs::remove_dir_all(bin).unwrap();
        fs::remove_dir_all(store_dir).unwrap();
    }

    #[test]
    fn tail_drains_buffered_lines_after_the_writer_goes_idle() {
        let path = unique_temp_path("poll-buffered-lines");
        fs::write(&path, "first\nsecond\n").unwrap();
        let mut position = 0;
        let mut pending = String::new();
        assert_eq!(
            tail_line(&path, &mut position, &mut pending).unwrap().text,
            "first"
        );
        let consumed_length = position;
        assert_eq!(
            tail_line(&path, &mut position, &mut pending).unwrap().text,
            "second"
        );
        assert_eq!(position, consumed_length);
        assert!(tail_line(&path, &mut position, &mut pending).is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn run_stems_separate_same_agent_missions() {
        let first = EnvironmentSessionId {
            project: "p".into(),
            mission: "curator-one".into(),
            generation: 1,
        };
        let second = EnvironmentSessionId {
            project: "p".into(),
            mission: "curator-two".into(),
            generation: 1,
        };
        assert_eq!(
            artifact_stem("shared-agent", Some(&first)),
            "shared-agent-curator-one-g1"
        );
        assert_ne!(
            artifact_stem("shared-agent", Some(&first)),
            artifact_stem("shared-agent", Some(&second))
        );
    }
}
