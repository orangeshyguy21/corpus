use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Resolve OpenCode without assuming a GUI-launched process inherited a useful
/// PATH. The explicit order is part of the launch contract: PATH, the user's
/// OpenCode installation, then the application resource tree.
pub(super) fn resolve_opencode() -> Result<PathBuf> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    let resources = crate::paths::resource_root_opt();
    resolve_opencode_from(path.as_deref(), home.as_deref(), resources.as_deref()).ok_or_else(|| {
        Error::Store(
            "opencode binary not found — tried PATH, ~/.opencode/bin/opencode, \
             .opencode/node_modules/.bin/opencode. Install it or put it on PATH."
                .into(),
        )
    })
}

/// Whether tmux exists and is new enough for the launch contract. Setting
/// `CORPUS_NO_TMUX=1` deliberately selects the piped fallback.
pub(super) fn tmux_available() -> Option<()> {
    if std::env::var("CORPUS_NO_TMUX").as_deref() == Ok("1") {
        return None;
    }
    let output = Command::new(resolve_tmux()?).arg("-V").output().ok()?;
    if output.status.success() && tmux_version_supported(&String::from_utf8_lossy(&output.stdout)) {
        Some(())
    } else {
        None
    }
}

/// Resolve tmux without assuming PATH, caching the immutable result for the
/// process. PATH wins over the common Homebrew, MacPorts, and system paths.
pub(super) fn resolve_tmux() -> Option<PathBuf> {
    static TMUX: OnceLock<Option<PathBuf>> = OnceLock::new();
    TMUX.get_or_init(|| {
        executable_on_path(std::env::var_os("PATH").as_deref(), "tmux").or_else(|| {
            [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/opt/local/bin",
                "/usr/bin",
            ]
            .iter()
            .map(|dir| PathBuf::from(dir).join("tmux"))
            .find(|candidate| is_executable(candidate))
        })
    })
    .clone()
}

fn resolve_opencode_from(
    path: Option<&OsStr>,
    home: Option<&OsStr>,
    resources: Option<&Path>,
) -> Option<PathBuf> {
    executable_on_path(path, "opencode")
        .or_else(|| {
            home.map(PathBuf::from)
                .map(|home| home.join(".opencode/bin/opencode"))
                .filter(|candidate| is_executable(candidate))
        })
        .or_else(|| {
            resources
                .map(|root| root.join(".opencode/node_modules/.bin/opencode"))
                .filter(|candidate| is_executable(candidate))
        })
}

/// Find an executable-ish file without running it; an OpenCode test double may
/// intentionally be a long-running script.
fn executable_on_path(path: Option<&OsStr>, name: &str) -> Option<PathBuf> {
    path.and_then(|path| {
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
        && fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

fn tmux_version_supported(output: &str) -> bool {
    let Some(field) = output.split_whitespace().nth(1) else {
        return false;
    };
    let field = field.trim_start_matches("next-");
    let Some(major) = field.split('.').next().and_then(|part| part.parse().ok()) else {
        return false;
    };
    let minor = field
        .split('.')
        .nth(1)
        .and_then(|part| {
            part.chars()
                .take_while(|character| character.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    (major, minor) >= (3_u32, 2_u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_path;

    fn executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn opencode_resolution_has_explicit_stable_precedence() {
        let world = unique_temp_path("launch-executable-precedence");
        let path_dir = world.join("path");
        let home = world.join("home");
        let resources = world.join("resources");
        let path_binary = path_dir.join("opencode");
        let home_binary = home.join(".opencode/bin/opencode");
        let resource_binary = resources.join(".opencode/node_modules/.bin/opencode");
        for binary in [&path_binary, &home_binary, &resource_binary] {
            executable(binary);
        }
        let search_path = std::env::join_paths([&path_dir]).unwrap();

        assert_eq!(
            resolve_opencode_from(Some(&search_path), Some(home.as_os_str()), Some(&resources)),
            Some(path_binary.clone())
        );
        fs::remove_file(&path_binary).unwrap();
        assert_eq!(
            resolve_opencode_from(Some(&search_path), Some(home.as_os_str()), Some(&resources)),
            Some(home_binary.clone())
        );
        fs::remove_file(&home_binary).unwrap();
        assert_eq!(
            resolve_opencode_from(Some(&search_path), Some(home.as_os_str()), Some(&resources)),
            Some(resource_binary)
        );
        fs::remove_dir_all(world).unwrap();
    }

    #[test]
    fn tmux_capability_accepts_32_and_newer_release_shapes() {
        assert!(!tmux_version_supported("tmux 3.1c\n"));
        assert!(tmux_version_supported("tmux 3.2\n"));
        assert!(tmux_version_supported("tmux 3.2a\n"));
        assert!(tmux_version_supported("tmux next-3.4\n"));
        assert!(tmux_version_supported("tmux 4.0\n"));
        assert!(!tmux_version_supported("tmux unknown\n"));
        assert!(!tmux_version_supported("broken\n"));
    }
}
