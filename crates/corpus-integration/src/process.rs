//! Bounded child-process helpers used by binary-level scenarios.

use std::io::{self, Write};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub fn json_lines(
    mut command: Command,
    requests: &[Value],
    timeout: Duration,
) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        for request in requests {
            serde_json::to_writer(&mut stdin, request).map_err(io::Error::other)?;
            stdin.write_all(b"\n")?;
        }
    }
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "child exceeded {timeout:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
