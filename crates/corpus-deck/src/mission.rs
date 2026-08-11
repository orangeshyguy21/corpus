//! mission.rs — local mission execution: spawn/tail/abort `opencode run`.
//!
//! The Missions view manages (at most) one live runner at a time. A
//! `Runner` owns the child process (or a replay thumb), a reader thread
//! streaming transcript lines over a channel, and an abort flag. Nothing
//! here touches the UI thread: the view drains lines on its own cadence.
//!
//! Local runs are tee'd to `store/runs/<ts>-<agent>-<slug>.log`, matching
//! the `corpus run` convention, so every local mission is corpus data.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

/// A live (or replaying) mission. Drop/abort kills the child.
#[derive(Debug)]
pub struct Runner {
    stop: Arc<AtomicBool>,
    rx: Receiver<String>,
    child: Option<Child>,
    tail: Option<JoinHandle<()>>,
    log_path: Option<PathBuf>,
    running: bool,
}

/// Real-opencode pacing for logs. Rough notes on the values used by a
/// live researcher session; replay converges to "looks like someone is
/// working" rather than "a file dumped at full speed".
const REPLAY_LINE_DELAY: std::time::Duration = std::time::Duration::from_millis(70);

impl Runner {
    /// Spawn a real local mission: `opencode run` with the given agent,
    /// optional model, and mission text. Transcript lines stream to the
    /// returned runner's channel *and* are tee'd to `log_path` (if any).
    pub fn spawn(agent: &str, model: Option<&str>, mission: &str) -> Result<Self, String> {
        let mut child = spawn_opencode(agent, model, mission)?;
        let stdout = child.stdout.take().ok_or("opencode gave no stdout")?;

        let log_path = make_run_log_path(agent, mission);
        let mut log_file = log_path
            .as_ref()
            .and_then(|p| std::fs::File::create(p).ok());

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        // Header, like `corpus run`: agent/model/started/mission.
        if let Some(file) = log_file.as_mut() {
            let started = now_epoch();
            let _ = write!(
                file,
                "# corpus run\n# agent: {agent}\n# model: {}\n# started: {started}\n# mission: {mission}\n\n",
                model.unwrap_or("(default)")
            );
        }

        let tail = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // EOF: opencode exited
                    Ok(_) => {
                        if let Some(file) = log_file.as_mut() {
                            let _ = file.write_all(line.as_bytes());
                            let _ = file.flush();
                        }
                        if tx.send(line.clone()).is_err() {
                            break; // viewer gone
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            stop,
            rx,
            child: Some(child),
            tail: Some(tail),
            log_path,
            running: true,
        })
    }

    /// Replay a stored run log at a realistic pace (no opencode process).
    /// Used both for the demo safety net and as a Mission in replay mode.
    pub fn replay(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);

        let tail = std::thread::spawn(move || {
            for line in lines {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                if tx.send(line).is_err() {
                    break;
                }
                std::thread::sleep(REPLAY_LINE_DELAY);
            }
        });

        Ok(Self {
            stop,
            rx,
            child: None,
            tail: Some(tail),
            log_path: Some(path.to_path_buf()),
            running: true,
        })
    }

    /// Drain any new transcript lines produced since the last call.
    pub fn poll(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(line) => out.push(line),
                Err(mpsc::TryRecvError::Empty) => break,
                // The sender dropped: the reader/replay thread ended (EOF,
                // abort, or replay done). The run is over.
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = false;
                    break;
                }
            }
        }
        out
    }

    /// Is the text still streaming?
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The run log path (local missions are auto-logged).
    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    /// Kill the child (and its process group on unix). The reader thread
    /// sees EOF and exits; `running` clears after the child is reaped.
    pub fn abort(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            {
                let pgid = child.id().to_string();
                let _ = Command::new("kill")
                    .args(["-TERM", &format!("-{pgid}")])
                    .status();
                let _ = child.kill();
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        // Kill + reap so no orphaned opencode or replay thread survives.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            {
                let pgid = child.id().to_string();
                let _ = Command::new("kill")
                    .args(["-TERM", &format!("-{pgid}")])
                    .status();
            }
            let _ = child.kill();
        }
        if let Some(tail) = self.tail.take() {
            let _ = tail.join();
        }
    }
}

/// Spawn `opencode run --agent <agent> [-m <model>] <mission>`.
fn spawn_opencode(agent: &str, model: Option<&str>, mission: &str) -> Result<Child, String> {
    let mut command = Command::new("opencode");
    command
        .args(["run", "--agent", agent])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;
    #[cfg(unix)]
    command.process_group(0);
    if let Some(model) = model {
        command.args(["-m", model]);
    }
    command
        .arg(mission)
        .spawn()
        .map_err(|e| format!("failed to spawn opencode (on PATH?): {e}"))
}

/// Build a `store/runs/<ts>-<agent>-<slug>.log` path for a local mission.
fn make_run_log_path(agent: &str, mission: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let store = std::env::var("CORPUS_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("{home}/Sites/corpus/store")));
    let runs = store.join("runs");
    if std::fs::create_dir_all(&runs).is_err() {
        return None;
    }
    let slug: String = mission
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    Some(runs.join(format!("{}-{agent}-{slug}.log", now_epoch())))
}

/// Current unix epoch seconds.
fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_log(lines: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("corpus-deck-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = dir.join(format!("replay-{n}.log"));
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn replay_streams_all_lines_then_finishes() {
        let lines = ["# corpus run", "> operator", "hello", "world"];
        let path = temp_log(&lines);
        let mut runner = Runner::replay(&path).expect("replay start");

        // Drain until the thread finishes. The pacing is 70 ms/line, so a
        // handful of lines resolves quickly.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = Vec::new();
        while runner.is_running() && std::time::Instant::now() < deadline {
            got.extend(runner.poll());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        got.extend(runner.poll()); // final partial buffer

        assert_eq!(got, lines, "every replayed line must arrive");
        assert!(!runner.is_running(), "replay must end on its own");
    }

    #[test]
    fn abort_stops_replay_early() {
        let lines: Vec<String> = (0..1000).map(|i| format!("line {i}")).collect();
        let src: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = temp_log(&src);
        let mut runner = Runner::replay(&path).expect("replay start");

        // Drain a little, then abort.
        let mut seen = 0;
        loop {
            let batch = runner.poll();
            seen += batch.len();
            if seen >= 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        runner.abort();
        std::thread::sleep(std::time::Duration::from_millis(50));

        // No more lines should arrive after an abort.
        let after = runner.poll();
        assert!(after.is_empty(), "aborted replay must stop: {after:?}");
    }
}

