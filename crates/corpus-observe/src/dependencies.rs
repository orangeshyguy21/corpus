//! Read-only readiness checks for host software the desktop application needs.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Corpus's verified OpenCode session/API contract.
pub const MINIMUM_OPENCODE_VERSION: &str = "1.18.18";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeReadiness {
    Ready {
        path: PathBuf,
        version: String,
    },
    Missing {
        message: String,
    },
    Incompatible {
        path: PathBuf,
        version: String,
        expected: &'static str,
    },
    Failed {
        path: PathBuf,
        message: String,
    },
}

impl OpenCodeReadiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// Resolve and verify OpenCode without blocking indefinitely on a damaged or
/// unexpected executable. Model/provider readiness is deliberately not part
/// of this application boot gate.
pub fn probe_opencode() -> OpenCodeReadiness {
    let path = match crate::resolve_opencode() {
        Ok(path) => path,
        Err(error) => {
            return OpenCodeReadiness::Missing {
                message: error.to_string(),
            }
        }
    };
    let output = match bounded_version_output(&path, Duration::from_secs(3)) {
        Ok(output) => output,
        Err(message) => return OpenCodeReadiness::Failed { path, message },
    };
    let version = output.trim().to_string();
    if is_compatible_opencode_version(&version) {
        OpenCodeReadiness::Ready { path, version }
    } else {
        OpenCodeReadiness::Incompatible {
            path,
            version,
            expected: MINIMUM_OPENCODE_VERSION,
        }
    }
}

/// The HTTP/session adapter is verified only for OpenCode 1.18.18 and newer
/// patches on the 1.18 line. A future minor release is a compatibility event,
/// not an ambient widening of the accepted surface.
pub fn is_compatible_opencode_version(version: &str) -> bool {
    let core = version
        .trim()
        .split_once('-')
        .map_or(version.trim(), |(core, _)| core);
    let mut parts = core.split('.');
    let parsed = (
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next(),
    );
    matches!(parsed, (Some(1), Some(18), Some(patch), None) if patch >= 18)
}

fn bounded_version_output(path: &std::path::Path, timeout: Duration) -> Result<String, String> {
    const OUTPUT_LIMIT: u64 = 8 * 1024;
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run OpenCode: {error}"))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(OUTPUT_LIMIT).read_to_end(&mut bytes);
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(OUTPUT_LIMIT).read_to_end(&mut bytes);
        bytes
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!(
                    "OpenCode did not answer --version within {} seconds",
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("could not inspect OpenCode: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "OpenCode stdout reader failed".to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "OpenCode stderr reader failed".to_string())?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("OpenCode --version exited with {status}")
        } else {
            format!("OpenCode --version failed: {detail}")
        });
    }
    let version = String::from_utf8_lossy(&stdout).trim().to_string();
    if version.is_empty() {
        Err("OpenCode --version returned no version".into())
    } else {
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_is_closed_to_the_verified_minor_line() {
        assert!(!is_compatible_opencode_version("1.18.17"));
        assert!(is_compatible_opencode_version("1.18.18"));
        assert!(is_compatible_opencode_version("1.18.20-beta.1"));
        assert!(!is_compatible_opencode_version("1.19.0"));
        assert!(!is_compatible_opencode_version("garbage"));
    }
}
