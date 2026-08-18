//! Source revision discovery, resolution, and on-demand fetch — the
//! multi-rev wiring behind the top-bar `repo: rev` dropdowns.
//!
//! `sources.toml` is the manifest of record for the DEFAULT pin per repo
//! (repo URL, tag, sha). Every rev the dropdown offers beyond the pin is
//! discovered live from the remote: `git ls-remote` for tags + the
//! `main`/`master` head, disk-cached under `sources/.rev-cache/` (a
//! derived, git-ignored cache — never trusted past its TTL unless the
//! network is gone, in which case a stale cache beats no list).
//!
//! A mission pin is a REV label (`v0.17.0`, `main`); launching resolves
//! it to a sha (the cache/ls-remote), fetches `sources/<name>/<sha>/`
//! when missing (the same depth-1 clone + sha-verify setup.sh does), and
//! hands the sha to the sandbox via `CORPUS_SOURCE_PINS` — the rev the
//! operator picked is the source the agent reads.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Disk-cache TTL for a remote's rev list (24h: tags are append-mostly,
/// and a stale cache only ever falls back gracefully).
pub const REV_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// The rev→sha cache file format (`sources/.rev-cache/<name>.json`).
#[derive(Debug, Serialize, Deserialize)]
struct RevCache {
    /// Epoch seconds when the refs were fetched.
    fetched: u64,
    /// Rev label (`v0.17.0`, `main`) → commit sha it points at.
    refs: BTreeMap<String, String>,
}

/// Whether a rev is a full commit sha — 40 lowercase hex chars. Such a
/// rev needs no name→sha resolution: it IS the sha, and `ensure_source_tree`
/// fetches it directly. Abbreviated shas are rejected on purpose (ambiguous,
/// and not fetchable by `git fetch <sha>`).
pub fn is_commit_sha(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The clone URL for a manifest `repo` value: `owner/name` shorthand goes
/// to github; anything carrying a scheme or path separator prefix is used
/// verbatim (tests point at local fixture repos).
fn remote_url(repo: &str) -> String {
    if repo.contains("://") || repo.starts_with('/') || repo.starts_with('.') {
        repo.to_string()
    } else {
        format!("https://github.com/{repo}.git")
    }
}

/// `git ls-remote` for tags + the main/master heads, mapping rev label →
/// sha. Annotated tags resolve to the peeled `^{}` commit (the sha a
/// clone at that tag checks out). Operators often rewrite github URLs in
/// a global gitconfig that cannot fetch these repos; the global config
/// is scrubbed for the call (empty file), same as setup.sh.
fn ls_remote(repo: &str) -> Result<BTreeMap<String, String>, Error> {
    let empty_cfg = std::env::temp_dir().join(format!("corpus-gitconfig-{}", std::process::id()));
    let _ = fs::write(&empty_cfg, "");
    let output = Command::new("git")
        .args(["ls-remote", "--tags", "--heads"])
        .arg(remote_url(repo))
        .env("GIT_CONFIG_GLOBAL", &empty_cfg)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| Error::Store(format!("git ls-remote failed to run: {e}")));
    let _ = fs::remove_file(&empty_cfg);
    let output = output?;
    if !output.status.success() {
        return Err(Error::Store(format!(
            "git ls-remote {repo} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mut refs: BTreeMap<String, String> = BTreeMap::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let (Some(sha), Some(reference)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Some(tag) = reference.strip_prefix("refs/tags/") {
            if let Some(peeled) = tag.strip_suffix("^{}") {
                refs.insert(peeled.to_string(), sha.to_string()); // annotated: commit sha wins
            } else {
                refs.entry(tag.to_string()).or_insert_with(|| sha.to_string());
            }
        } else if reference == "refs/heads/main" || reference == "refs/heads/master" {
            let name = reference.trim_start_matches("refs/heads/");
            refs.insert(name.to_string(), sha.to_string());
        }
    }
    Ok(refs)
}

/// Read the cached refs for a repo source: fresh cache used directly;
/// stale cache refreshed over the network (falling back to the stale
/// cache when the refresh fails); no cache at all is fetched live.
/// Returns None when nothing (network + no cache) is available.
fn cached_refs(sources_dir: &Path, name: &str, repo: &str) -> Option<BTreeMap<String, String>> {
    let now = now_secs();
    let path = cache_path(sources_dir, name);
    let cache: Option<RevCache> = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    if let Some(cache) = &cache {
        if now.saturating_sub(cache.fetched) < REV_CACHE_TTL_SECS && !cache.refs.is_empty() {
            return Some(cache.refs.clone());
        }
    }
    match ls_remote(repo) {
        Ok(refs) if !refs.is_empty() => {
            let fresh = RevCache {
                fetched: now,
                refs: refs.clone(),
            };
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&fresh) {
                let _ = fs::write(&path, json);
            }
            Some(refs)
        }
        _ => cache.map(|c| c.refs).filter(|refs| !refs.is_empty()),
    }
}

/// `sources/.rev-cache/<name>.json` — derived cache beside the fetched trees.
fn cache_path(sources_dir: &Path, name: &str) -> PathBuf {
    sources_dir.join(".rev-cache").join(format!("{name}.json"))
}

/// Epoch seconds when a source's rev list was last fetched live (None when
/// there is no cache). The UI surfaces this so a branch rev (`main`) pulled
/// from a stale cache is visible to the operator rather than silently
/// frozen at yesterday's head.
pub fn revs_cache_fetched(sources_dir: &Path, name: &str) -> Option<u64> {
    let raw = fs::read_to_string(cache_path(sources_dir, name)).ok()?;
    let cache: RevCache = serde_json::from_str(&raw).ok()?;
    Some(cache.fetched)
}

/// Every selectable rev for a source, ordered with the DEFAULT first:
/// `main` leads when its sha is actually known (the remote answered or a
/// cache exists), then the manifest pin, then the remaining tags
/// newest-first (version-aware sort), deduped. Offline-with-no-cache
/// degrades to the pin + `main` with the PIN leading — a default must be
/// resolvable without the network.
pub fn selectable_revs(sources_dir: &Path, name: &str, repo: &str, pinned: &str) -> Vec<String> {
    let Some(refs) = cached_refs(sources_dir, name, repo) else {
        let mut revs = vec![pinned.to_string()];
        if pinned != "main" {
            revs.push("main".to_string());
        }
        return revs;
    };
    let mut out: Vec<String> = Vec::new();
    for head in ["main", "master"] {
        if refs.contains_key(head) {
            out.push(head.to_string());
            break; // one leading branch default
        }
    }
    if !out.iter().any(|r| r == pinned) {
        out.push(pinned.to_string());
    }
    let mut tags: Vec<&String> = refs
        .keys()
        .filter(|k| k.as_str() != "main" && k.as_str() != "master")
        .collect();
    tags.sort_by(|a, b| version_key(b).cmp(&version_key(a))); // newest first
    out.extend(tags.into_iter().filter(|t| *t != pinned).cloned());
    out
}

/// Resolve a rev label to the commit sha it currently points at.
/// `main`/`master` resolve to the cached/discovered head — recorded on
/// the mission at launch, so a moving branch is pinned at pick time.
pub fn resolve_rev(
    sources_dir: &Path,
    name: &str,
    repo: &str,
    rev: &str,
) -> Result<String, Error> {
    let refs = cached_refs(sources_dir, name, repo).ok_or_else(|| {
        Error::Store(format!(
            "no rev data for {name} (offline and no cache) — cannot resolve {rev:?}"
        ))
    })?;
    refs.get(rev).cloned().ok_or_else(|| {
        Error::Store(format!(
            "unknown rev {rev:?} for {name} — known: {}",
            refs.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    })
}

/// Ensure `sources/<name>/<sha>/` exists (fetching it when missing),
/// returning the tree path. Same discipline as setup.sh: depth-1 clone at
/// the rev label, then verify HEAD is exactly the resolved sha. A branch
/// that moved between resolve and clone falls back to fetching the sha
/// directly (remotes with allowAnySHA1InWant, e.g. GitHub); only when
/// that fails is the pin a loud error — never a silently wrong mount.
pub fn ensure_source_tree(
    sources_dir: &Path,
    name: &str,
    repo: &str,
    rev: &str,
    sha: &str,
) -> Result<PathBuf, Error> {
    let dest = sources_dir.join(name).join(sha);
    if head_matches(&dest, sha) {
        return Ok(dest);
    }
    let tmp = sources_dir.join(name).join(format!(".fetch-{sha}"));
    let _ = fs::remove_dir_all(&tmp);
    let empty_cfg = std::env::temp_dir().join(format!("corpus-gitconfig-{}", std::process::id()));
    let _ = fs::write(&empty_cfg, "");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &empty_cfg)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if is_commit_sha(rev) {
        // A bare sha is not a valid `--branch`: init an empty repo and
        // fetch the commit directly (GitHub serves an arbitrary sha via
        // allowAnySHA1InWant). This is the path a mission pinned to an
        // exact commit takes — `resolve_rev` is skipped upstream because
        // there is no name to resolve.
        let dir = tmp.to_string_lossy().into_owned();
        let ok = fs::create_dir_all(&tmp).is_ok()
            && git(&["-C", &dir, "init", "--quiet"])
            && git(&["-C", &dir, "remote", "add", "origin", &remote_url(repo)])
            && git(&["-C", &dir, "fetch", "--quiet", "--depth", "1", "origin", sha])
            && git(&["-C", &dir, "checkout", "--quiet", "--detach", sha]);
        let _ = fs::remove_file(&empty_cfg);
        if !ok || !head_matches(&tmp, sha) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::Store(format!(
                "fetch failed: {repo}@{sha} (network? sha not on the remote?)"
            )));
        }
    } else {
        let status = git(&[
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--branch",
            rev,
            &remote_url(repo),
            &tmp.to_string_lossy(),
        ]);
        let _ = fs::remove_file(&empty_cfg);
        if !status {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::Store(format!(
                "clone failed: {repo}@{rev} (network? tag gone?)"
            )));
        }
        if !head_matches(&tmp, sha) {
            // The rev moved between resolve and clone (a branch tip) — fetch
            // the recorded sha itself and check it out.
            let dir = tmp.to_string_lossy().into_owned();
            let fetched = git(&["-C", &dir, "fetch", "--quiet", "--depth", "1", "origin", sha])
                && git(&["-C", &dir, "checkout", "--quiet", "--detach", sha]);
            if !fetched || !head_matches(&tmp, sha) {
                let got = head(&tmp).unwrap_or_else(|| "unknown".into());
                let _ = fs::remove_dir_all(&tmp);
                return Err(Error::Store(format!(
                    "{name}@{rev} moved: expected {sha}, got {got}, and the sha itself is not fetchable — re-pick the rev"
                )));
            }
        }
    }
    let _ = fs::remove_dir_all(&dest);
    fs::rename(&tmp, &dest).map_err(|e| {
        let _ = fs::remove_dir_all(&tmp);
        Error::Store(format!("install sources/{name}/{sha}: {e}"))
    })?;
    Ok(dest)
}

/// The tree's HEAD equals the sha (a previously fetched, intact checkout).
fn head_matches(tree: &Path, sha: &str) -> bool {
    head(tree).as_deref() == Some(sha)
}

fn head(tree: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", &tree.to_string_lossy(), "rev-parse", "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Version-aware ordering key: numeric runs compare numerically, so
/// v0.17.0 sorts after v0.9.2; non-numeric tail compares lexically.
fn version_key(rev: &str) -> Vec<u64> {
    rev.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_sha_is_exactly_40_lowercase_hex() {
        assert!(is_commit_sha("86a7c6cacb362daa67a0d636e303b66faf3965d9"));
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(!is_commit_sha("86A7C6CACB362DAA67A0D636E303B66FAF3965D9")); // uppercase
        assert!(!is_commit_sha("86a7c6c")); // abbreviated
        assert!(!is_commit_sha(&"a".repeat(39)));
        assert!(!is_commit_sha(&"a".repeat(41)));
        assert!(!is_commit_sha("v0.18.0-rc.0")); // a tag name
        assert!(!is_commit_sha("main"));
        assert!(!is_commit_sha("")); // empty
        assert!(!is_commit_sha(&format!("{}g", "a".repeat(39)))); // non-hex
    }

    /// Build a bare fixture repo with two tags (one annotated) and a main
    /// branch; returns its path for `ls-remote`/clone over the file path.
    fn fixture_repo(tag: &str, tag2: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "corpus-srcrev-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(&work)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet", "-b", "main"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--quiet", "--allow-empty", "-m", "one"]);
        git(&["tag", tag]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--quiet", "--allow-empty", "-m", "two"]);
        git(&["tag", "-a", tag2, "-m", "annotated"]);
        work
    }

    #[test]
    fn ls_remote_maps_tags_and_heads() {
        let repo = fixture_repo("v0.1.0", "v0.2.0");
        let refs = ls_remote(repo.to_str().unwrap()).unwrap();
        assert!(refs.contains_key("main"), "main head present: {refs:?}");
        assert!(refs.contains_key("v0.1.0"));
        assert!(refs.contains_key("v0.2.0"));
        // Annotated tag resolves to the commit (== main head here), not
        // the tag object.
        assert_eq!(refs.get("v0.2.0"), refs.get("main"));
        let _ = fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn selectable_orders_pin_main_tags_desc() {
        let repo = fixture_repo("v9.9.9", "v0.2.0");
        let dir = std::env::temp_dir().join(format!("corpus-srcrev-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let repo = repo.to_str().unwrap().to_string();
        let revs = selectable_revs(&dir, "t", &repo, "v0.1.0");
        // main first (the default), then the pin, then tags newest-first
        // (v9.9.9 > v0.2.0).
        assert_eq!(
            revs,
            vec![
                "main".to_string(),
                "v0.1.0".to_string(),
                "v9.9.9".to_string(),
                "v0.2.0".to_string()
            ]
        );
        // Resolve + fetch round-trip: the pinned tag materializes at its sha.
        let sha = resolve_rev(&dir, "t", &repo, "v0.2.0").unwrap();
        assert_eq!(sha.len(), 40);
        let tree = ensure_source_tree(&dir, "t", &repo, "v0.2.0", &sha).unwrap();
        assert!(tree.join(".git").exists());
        assert!(head_matches(&tree, &sha));
        assert!(resolve_rev(&dir, "t", &repo, "v8.8.8").is_err());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(Path::new(&repo).parent().unwrap());
    }
}
