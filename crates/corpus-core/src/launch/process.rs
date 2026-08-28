//! Owned child-process supervision for launch and export subprocesses.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::RunLine;
use crate::error::{Error, Result};

pub(super) struct BoundedOutputSpec {
    operation: &'static str,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
}

impl BoundedOutputSpec {
    pub(super) fn new(operation: &'static str, timeout: Duration, stdout_limit: usize) -> Self {
        Self {
            operation,
            timeout,
            stdout_limit,
            stderr_limit: 64 * 1024,
        }
    }
}

#[derive(Debug)]
pub(super) struct ManagedOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) stdout_truncated: bool,
}

/// Spawn a piped run only after its durable transcript is open, then pump both
/// child streams into the transcript and a typed line channel.
pub(super) fn spawn_piped(
    mut command: Command,
    transcript: &Path,
) -> Result<(Child, Receiver<RunLine>)> {
    let log = OpenOptions::new().append(true).open(transcript)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    own_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| Error::Store(format!("failed to spawn opencode (on PATH?): {error}")))?;
    let Some(stdout) = child.stdout.take() else {
        let cleanup = kill_tree_checked(&mut child);
        return Err(missing_stream_error("stdout", &cleanup));
    };
    let Some(stderr) = child.stderr.take() else {
        let cleanup = kill_tree_checked(&mut child);
        return Err(missing_stream_error("stderr", &cleanup));
    };
    let (tx, rx) = mpsc::channel();
    let log = Arc::new(Mutex::new(log));
    pump(stdout, false, tx.clone(), Arc::clone(&log));
    pump(stderr, true, tx, log);
    Ok((child, rx))
}

fn missing_stream_error(stream: &str, cleanup: &[String]) -> Error {
    let suffix = if cleanup.is_empty() {
        String::new()
    } else {
        format!("; cleanup: {}", cleanup.join("; "))
    };
    Error::Store(format!("no {stream} from opencode{suffix}"))
}

/// Run a subprocess with a deadline and retained-output caps. Timeout and wait
/// failure kill the entire owned process group and reap it before returning.
pub(super) fn bounded_command_output(
    mut command: Command,
    spec: BoundedOutputSpec,
) -> Result<ManagedOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    own_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| Error::Store(format!("{} failed: {error}", spec.operation)))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Store(format!("{} stdout was not captured", spec.operation)))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Store(format!("{} stderr was not captured", spec.operation)))?;
    let stdout_limit = spec.stdout_limit;
    let stderr_limit = spec.stderr_limit;
    let stdout_reader = std::thread::spawn(move || read_stream_capped(stdout, stdout_limit));
    let stderr_reader = std::thread::spawn(move || read_stream_capped(stderr, stderr_limit));

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < spec.timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let cleanup = kill_tree_checked(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(Error::Store(timeout_error(&spec, &cleanup)));
            }
            Err(error) => {
                let cleanup = kill_tree_checked(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(Error::Store(format!(
                    "cannot wait for {}: {error}; cleanup: {}",
                    spec.operation,
                    cleanup.join("; ")
                )));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| Error::Store(format!("{} stdout reader panicked", spec.operation)))??;
    let (stderr, _) = stderr_reader
        .join()
        .map_err(|_| Error::Store(format!("{} stderr reader panicked", spec.operation)))??;
    Ok(ManagedOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
    })
}

fn timeout_error(spec: &BoundedOutputSpec, cleanup: &[String]) -> String {
    let prefix = format!(
        "{} timed out after {}s",
        spec.operation,
        spec.timeout.as_secs_f32()
    );
    if cleanup.is_empty() {
        prefix
    } else {
        format!("{prefix}; cleanup: {}", cleanup.join("; "))
    }
}

fn own_process_group(command: &mut Command) {
    command.process_group(0);
}

fn pump<R>(
    stream: R,
    stderr: bool,
    tx: Sender<RunLine>,
    log: Arc<Mutex<fs::File>>,
) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if let Ok(mut log) = log.lock() {
                let _ = writeln!(log, "{line}");
            }
            let _ = tx.send(RunLine { stderr, text: line });
        }
    })
}

fn read_stream_capped(mut stream: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::with_capacity(limit.min(16 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((retained, truncated))
}

/// Kill a child and its entire owned process group, then reap the child.
pub(super) fn kill_tree_checked(child: &mut Child) -> Vec<String> {
    let mut errors = Vec::new();
    match child.try_wait() {
        Ok(Some(_)) => return errors,
        Ok(None) => {}
        Err(error) => errors.push(format!(
            "poll child {} before process-group cleanup: {error}",
            child.id()
        )),
    }
    let pgid = child.id().to_string();
    if let Err(error) = signal_group("-TERM", &pgid) {
        errors.push(format!("signal process group {pgid}: {error}"));
    }
    let grace_started = Instant::now();
    while grace_started.elapsed() < Duration::from_millis(150) {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                errors.push(format!("poll child {} during cleanup: {error}", child.id()));
                break;
            }
        }
    }
    if let Err(error) = signal_group("-KILL", &pgid) {
        errors.push(format!("kill process group {pgid}: {error}"));
    }
    if let Err(error) = child.kill() {
        if error.kind() != std::io::ErrorKind::InvalidInput {
            errors.push(format!("kill child {}: {error}", child.id()));
        }
    }
    if let Err(error) = child.wait() {
        errors.push(format!("reap child {}: {error}", child.id()));
    }
    errors
}

fn signal_group(signal: &str, pgid: &str) -> std::io::Result<()> {
    Command::new("kill")
        .args([signal, &format!("-{pgid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
}

pub(super) fn stopped_exit_status() -> ExitStatus {
    ExitStatus::from_raw(130 << 8)
}

pub(super) fn successful_exit_status() -> ExitStatus {
    ExitStatus::from_raw(0)
}

#[cfg(test)]
mod tests {
    use super::super::command::shell_quote;
    use super::*;
    use crate::test_support::unique_temp_path;

    #[test]
    fn bounded_output_caps_stdout_and_keeps_stderr_separate() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 123456789; printf problem >&2"]);
        let output = bounded_command_output(
            command,
            BoundedOutputSpec::new("fixture", Duration::from_secs(1), 5),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"12345");
        assert_eq!(output.stderr, b"problem");
        assert!(output.stdout_truncated);
    }

    #[test]
    fn piped_spawn_publishes_typed_lines_and_a_durable_log() {
        let transcript = unique_temp_path("launch-process-pump");
        fs::write(&transcript, "header\n").unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'answer\\n'; printf 'warning\\n' >&2"]);
        let (mut child, rx) = spawn_piped(command, &transcript).unwrap();
        assert!(child.wait().unwrap().success());
        let mut lines = [
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        ];
        lines.sort_by_key(|line| line.stderr);
        assert_eq!(lines[0].text, "answer");
        assert!(!lines[0].stderr);
        assert_eq!(lines[1].text, "warning");
        assert!(lines[1].stderr);
        let log = fs::read_to_string(&transcript).unwrap();
        assert!(log.starts_with("header\n"));
        assert!(log.contains("answer\n"));
        assert!(log.contains("warning\n"));
        fs::remove_file(transcript).unwrap();
    }

    #[test]
    #[ignore = "platform: sends signals to an owned Unix process group"]
    fn timeout_kills_and_reaps_the_owned_process_group() {
        let pid_file = unique_temp_path("launch-process-timeout-pid");
        let quoted_pid = shell_quote(&pid_file.to_string_lossy());
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(format!("printf '%s' \"$$\" > {quoted_pid}; sleep 30"));
        let started = Instant::now();
        let error = bounded_command_output(
            command,
            BoundedOutputSpec::new("fixture", Duration::from_millis(150), 1024),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = fs::read_to_string(&pid_file).unwrap();
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "timed-out process {pid} still exists");
        fs::remove_file(pid_file).unwrap();
    }
}
