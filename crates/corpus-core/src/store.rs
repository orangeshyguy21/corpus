//! The scoped corpus store.
//!
//! On-disk layout (data-model plan):
//!
//! ```text
//! store/
//!   templates/                 # CORE templates (versioned with the app)
//!     permissions/ prompts/ agents/
//!   projects/<project-slug>/
//!     project.yaml             # name, plugin binding, created/cloned-from
//!     templates/               # user + plugin-imported templates (same 3 dirs)
//!     teams/<team-slug>/
//!       team.yaml              # agents, rev override, corpus_generation
//!       corpus/                # team-scoped store (same category layout)
//!     corpus/                  # project-global corpus (promoted entries)
//! ```
//!
//! The old flat `store/{hypotheses,techniques,findings,attacks,runs}`
//! becomes `store/projects/<default>/corpus/` via a migration that relocates
//! files verbatim. Wiki-truth: markdown + YAML frontmatter is truth, this is
//! all filesystem plumbing, no DB.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::frontmatter;
use crate::sensitivity::Sensitivity;
use crate::templates::Templates;

/// Project the flat store migrates into, and the unscoped default.
pub const DEFAULT_PROJECT_SLUG: &str = "default";
/// Backward-compat team: unscoped MCP writes target this team.
pub const DEFAULT_TEAM_SLUG: &str = "default";

/// Environment variables overriding the default scope.
pub const STORE_ENV: &str = "CORPUS_STORE";
pub const PROJECT_ENV: &str = "CORPUS_PROJECT";
pub const TEAM_ENV: &str = "CORPUS_TEAM";

/// The corpus category layout, shared by project corpora and team corpora.
pub const CATEGORIES: [&str; 5] = ["hypotheses", "techniques", "findings", "attacks", "runs"];

/// Resolve the store root: `CORPUS_STORE`, else `~/Sites/corpus/store`.
pub fn store_root_env() -> PathBuf {
    std::env::var(STORE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(format!("{home}/Sites/corpus/store"))
        })
}

/// The current project scope: `CORPUS_PROJECT` else `default`.
pub fn project_slug_env() -> String {
    std::env::var(PROJECT_ENV).unwrap_or_else(|_| DEFAULT_PROJECT_SLUG.to_string())
}

/// The current team scope: `CORPUS_TEAM` else `default`.
pub fn team_slug_env() -> String {
    std::env::var(TEAM_ENV).unwrap_or_else(|_| DEFAULT_TEAM_SLUG.to_string())
}

/// The store: path plumbing over the scoped layout.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_env() -> Self {
        Self::new(store_root_env())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The core template set (read-only, versioned with the app).
    pub fn core_templates(&self) -> Templates {
        Templates::at(&self.root.join("templates"))
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn project_dir(&self, slug: &str) -> PathBuf {
        self.projects_dir().join(slug)
    }

    pub fn project_templates(&self, slug: &str) -> Templates {
        Templates::at(&self.project_dir(slug).join("templates"))
    }

    pub fn project_templates_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("templates")
    }

    pub fn project_teams_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("teams")
    }

    /// The project-global corpus (curated; team entries are promoted here).
    pub fn project_corpus_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("corpus")
    }

    pub fn team_dir(&self, project: &str, team: &str) -> PathBuf {
        self.project_dir(project).join("teams").join(team)
    }

    pub fn team_corpus_dir(&self, project: &str, team: &str) -> PathBuf {
        self.team_dir(project, team).join("corpus")
    }
}

/// The current write/promote scope: which project's team corpus writes land
/// in. Unscoped tools resolve here; explicit `team` arguments override the
/// team half.
#[derive(Debug, Clone)]
pub struct Scope {
    pub project: String,
    pub team: String,
}

impl Scope {
    pub fn new(project: impl Into<String>, team: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            team: team.into(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(project_slug_env(), team_slug_env())
    }

    /// The team corpus directory this scope writes to.
    pub fn corpus_dir(&self, store: &Store) -> PathBuf {
        store.team_corpus_dir(&self.project, &self.team)
    }

    /// The project-global corpus directory (promotion destination).
    pub fn project_corpus_dir(&self, store: &Store) -> PathBuf {
        store.project_corpus_dir(&self.project)
    }

    /// Runs directories for the run_log gate: the team corpus first, the
    /// project-global corpus as fallback so run logs that migrated with the
    /// flat store stay resolvable.
    pub fn runs_dirs(&self, store: &Store) -> [PathBuf; 2] {
        [
            store.team_corpus_dir(&self.project, &self.team).join("runs"),
            store.project_corpus_dir(&self.project).join("runs"),
        ]
    }
}

/// A project (`store/projects/<slug>/project.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// The environment plugin this project is bound to.
    pub plugin: String,
    /// Epoch seconds of creation.
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
}

impl Project {
    pub fn load(store: &Store, slug: &str) -> Result<Self> {
        let path = store.project_dir(slug).join("project.yaml");
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("project not found: {slug}")))?;
        let project: Project = serde_yaml::from_str(&raw)
            .map_err(|e| Error::Store(format!("project {slug}: {e}")))?;
        Ok(project)
    }

    fn save(&self, store: &Store, slug: &str) -> Result<()> {
        let path = store.project_dir(slug).join("project.yaml");
        let raw = serde_yaml::to_string(self)?;
        fs::write(path, raw)?;
        Ok(())
    }
}

/// An agent instantiated from a template inside a team spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInstance {
    /// Slug of the agent template (resolved user→core).
    pub template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
}

/// A team spec (`store/projects/<p>/teams/<t>/team.yaml`). Teams are
/// configs (roadmap §2): agents plus a pinned-rev override plus a corpus
/// generation counter.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamSpec {
    /// Human label.
    pub name: String,
    /// Agent instantiations, keyed by agent name.
    #[serde(default)]
    pub agents: BTreeMap<String, AgentInstance>,
    /// Optional pinned-rev override; defaults to the plugin's pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev_override: Option<String>,
    /// Bumped on every corpus wipe so old run logs stay attributable.
    #[serde(default)]
    pub corpus_generation: u64,
    #[serde(default)]
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
}

impl TeamSpec {
    pub fn load(store: &Store, project: &str, team: &str) -> Result<Self> {
        let path = store.team_dir(project, team).join("team.yaml");
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("team not found: {project}/{team}")))?;
        let spec: TeamSpec = serde_yaml::from_str(&raw)
            .map_err(|e| Error::Store(format!("team {project}/{team}: {e}")))?;
        Ok(spec)
    }

    fn save(&self, store: &Store, project: &str, team: &str) -> Result<()> {
        let path = store.team_dir(project, team).join("team.yaml");
        let raw = serde_yaml::to_string(self)?;
        fs::write(path, raw)?;
        Ok(())
    }

    /// `project/team@hash/generation`: stable attribution for entries the
    /// team produced. Hash is a content hash of this spec's YAML.
    pub fn provenance(&self, project: &str, team: &str) -> String {
        format!(
            "{project}/{team}@{}/{}",
            fnv1a_hex(serde_yaml::to_string(self).unwrap_or_default().as_bytes()),
            self.corpus_generation
        )
    }
}

/// A FNV-1a 64-bit content hash, hex-encoded. Deliberately dependency-free:
/// provenance needs a stable, collision-resistant-enough attribution string,
/// not a cryptographic signature (that lands with #17 store hardening).
pub fn fnv1a_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a(bytes))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Validate a project or team slug: kebab-case, no path escapes.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 64
        || slug == "."
        || slug == ".."
        || !slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Error::Store(format!(
            "invalid slug {slug:?}: kebab-case alphanumerics only (a-z0-9-)"
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(Error::Store(format!("invalid slug {slug:?}: no leading/trailing dashes")));
    }
    Ok(())
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// -------------------------------------------------------------------------
// Projects
// -------------------------------------------------------------------------

impl Store {
    /// Create a project: `store/projects/<slug>/` with template dirs, teams
    /// dir, and an empty project-global corpus.
    pub fn create_project(&self, slug: &str, name: &str, plugin: &str) -> Result<Project> {
        validate_slug(slug)?;
        let dir = self.project_dir(slug);
        if dir.exists() {
            return Err(Error::Store(format!("project already exists: {slug}")));
        }
        fs::create_dir_all(&dir)?;
        let project = Project {
            name: name.to_string(),
            plugin: plugin.to_string(),
            created: now_epoch(),
            cloned_from: None,
        };
        self.project_templates(slug).ensure()?;
        fs::create_dir_all(self.project_teams_dir(slug))?;
        for category in CATEGORIES {
            fs::create_dir_all(self.project_corpus_dir(slug).join(category))?;
        }
        project.save(self, slug)?;
        Ok(project)
    }

    /// List projects, sorted by slug.
    pub fn list_projects(&self) -> Result<Vec<(String, Project)>> {
        let mut found = Vec::new();
        let dir = self.projects_dir();
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Store("non-utf8 project dir".into()))?;
            if let Ok(project) = Project::load(self, slug) {
                found.push((slug.to_string(), project));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// Delete a project (removes its whole subtree).
    pub fn delete_project(&self, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_dir(slug);
        if !dir.is_dir() {
            return Err(Error::Store(format!("project not found: {slug}")));
        }
        if slug == DEFAULT_PROJECT_SLUG {
            return Err(Error::Store("refusing to delete the default project".into()));
        }
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// Clone a project: config + templates, corpus copy optional. Teams are
    /// not cloned (a fresh project starts with an empty team list).
    pub fn clone_project(
        &self,
        from: &str,
        to: &str,
        name: Option<&str>,
        with_corpus: bool,
    ) -> Result<Project> {
        let source = Project::load(self, from)?;
        self.create_project(to, name.unwrap_or(&source.name), &source.plugin)?;
        let project = Project {
            name: name.unwrap_or(&source.name).to_string(),
            cloned_from: Some(from.to_string()),
            ..source
        };
        project.save(self, to)?;
        copy_tree(
            &self.project_templates_dir(from),
            &self.project_templates_dir(to),
        )?;
        if with_corpus {
            copy_tree(
                &self.project_corpus_dir(from),
                &self.project_corpus_dir(to),
            )?;
        }
        Ok(project)
    }
}

// -------------------------------------------------------------------------
// Teams + scoped corpora
// -------------------------------------------------------------------------

/// The agents that ship in the two core templates — reused for the default
/// team (backward-compat unscoped scope) and referenced by tests.
pub fn core_agent_instances() -> BTreeMap<String, AgentInstance> {
    let mut agents = BTreeMap::new();
    agents.insert(
        "operator".to_string(),
        AgentInstance {
            template: "operator".to_string(),
            model: None,
            budget: None,
        },
    );
    agents.insert(
        "researcher".to_string(),
        AgentInstance {
            template: "researcher".to_string(),
            model: None,
            budget: None,
        },
    );
    agents
}

impl Store {
    /// Create a team from agent instantiations, with a pristine corpus.
    pub fn create_team(
        &self,
        project: &str,
        slug: &str,
        name: &str,
        agents: BTreeMap<String, AgentInstance>,
        rev_override: Option<&str>,
    ) -> Result<TeamSpec> {
        validate_slug(slug)?;
        // The project must exist to host the team.
        if !self.project_dir(project).join("project.yaml").is_file() {
            return Err(Error::Store(format!("project not found: {project}")));
        }
        let dir = self.team_dir(project, slug);
        if dir.exists() {
            return Err(Error::Store(format!("team already exists: {project}/{slug}")));
        }
        let spec = TeamSpec {
            name: name.to_string(),
            agents,
            rev_override: rev_override.map(str::to_string),
            corpus_generation: 0,
            created: now_epoch(),
            cloned_from: None,
        };
        fs::create_dir_all(&dir)?;
        ensure_corpus_categories(&self.team_corpus_dir(project, slug))?;
        spec.save(self, project, slug)?;
        Ok(spec)
    }

    /// List a project's teams, sorted by slug.
    pub fn list_teams(&self, project: &str) -> Result<Vec<(String, TeamSpec)>> {
        let mut found = Vec::new();
        let dir = self.project_teams_dir(project);
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() || !path.join("team.yaml").is_file() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Store("non-utf8 team dir".into()))?;
            if let Ok(spec) = TeamSpec::load(self, project, slug) {
                found.push((slug.to_string(), spec));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// Update a team's spec in place (label, agents, rev override).
    pub fn update_team(
        &self,
        project: &str,
        slug: &str,
        f: impl FnOnce(&mut TeamSpec) -> Result<()>,
    ) -> Result<TeamSpec> {
        let mut spec = TeamSpec::load(self, project, slug)?;
        f(&mut spec)?;
        spec.save(self, project, slug)?;
        Ok(spec)
    }

    /// Delete a team (removes its spec and corpus subtree).
    pub fn delete_team(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.team_dir(project, slug);
        if !dir.is_dir() {
            return Err(Error::Store(format!("team not found: {project}/{slug}")));
        }
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// Clone a team: spec + full corpus deep copy. The clone keeps the
    /// source's `corpus_generation` (a snapshot); wiping the source later
    /// increments only the source's counter and leaves the clone untouched.
    pub fn clone_team(&self, project: &str, from: &str, to: &str) -> Result<(String, TeamSpec)> {
        validate_slug(to)?;
        let source = TeamSpec::load(self, project, from)?;
        let dir = self.team_dir(project, to);
        if dir.exists() {
            return Err(Error::Store(format!("team already exists: {project}/{to}")));
        }
        let spec = TeamSpec {
            name: source.name.clone(),
            cloned_from: Some(from.to_string()),
            ..source
        };
        fs::create_dir_all(&dir)?;
        copy_tree(
            &self.team_corpus_dir(project, from),
            &self.team_corpus_dir(project, to),
        )?;
        spec.save(self, project, to)?;
        Ok((to.to_string(), spec))
    }

    /// Wipe a team's corpus: delete the working subtree, keep the spec, bump
    /// `corpus_generation`. Fresh runs lose no provenance — the wipe is a
    /// working-tree operation and the generation keeps old logs attributable.
    pub fn wipe_team_corpus(&self, project: &str, slug: &str) -> Result<TeamSpec> {
        let mut spec = TeamSpec::load(self, project, slug)?;
        let corpus = self.team_corpus_dir(project, slug);
        if corpus.is_dir() {
            fs::remove_dir_all(&corpus)?;
        }
        ensure_corpus_categories(&corpus)?;
        spec.corpus_generation += 1;
        spec.save(self, project, slug)?;
        Ok(spec)
    }
}

/// Create the category directories under a corpus root.
fn ensure_corpus_categories(corpus: &Path) -> Result<()> {
    for category in CATEGORIES {
        fs::create_dir_all(corpus.join(category))?;
    }
    Ok(())
}

/// Recursively copy a directory tree, preserving file permissions.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        fs::create_dir_all(dst)?;
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Flat-store migration
// -------------------------------------------------------------------------

/// What the flat-store migration did.
#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    /// Entries relocated and checksum-verified after the rename.
    pub moved: Vec<PathBuf>,
    /// Source entries whose scoped destination already held a copy — never
    /// overwritten, left where they were.
    pub skipped: Vec<PathBuf>,
    /// Entries that survived the rename but FAILED post-move checksum
    /// verification. Fetch these back up before anything is declared done.
    pub unverified: Vec<PathBuf>,
    /// Entries that would move in a dry run (nothing changed).
    pub would_move: Vec<PathBuf>,
    /// Legacy category directories removed — only ever after every entry
    /// they held moved with a verifiable checksum.
    pub removed_categories: Vec<String>,
    /// True when the report came from a dry run (no mutations).
    pub dry_run: bool,
}

/// Migration knobs.
#[derive(Debug, Clone, Default)]
pub struct MigrateOptions {
    /// Report only; make no changes to the store.
    pub dry_run: bool,
}

impl Store {
    /// See [`Store::migrate_legacy_flat_opt`].
    pub fn migrate_legacy_flat(&self, project: &str) -> Result<MigrationReport> {
        self.migrate_legacy_flat_opt(project, MigrateOptions::default())
    }

    /// Migrate the legacy flat `store/{categories}` layout into
    /// `store/projects/<project>/corpus/`. Hardened so this class of
    /// accident is impossible:
    ///
    /// - files move with a same-filesystem `rename` (atomic, byte-exact);
    /// - every moved entry is checksummed BEFORE the move; the destination
    ///   is checksummed AFTER it and compared — mismatches are reported in
    ///   `unverified`, never silently accepted;
    /// - a legacy category directory is removed only when EVERY entry it
    ///   held relocated with a verified checksum (or the dir was already
    ///   empty) — never when a move went unverified or an entry was skipped;
    /// - a pre-populated destination is never overwritten: the source entry
    ///   is left in place and reported as skipped;
    /// - `dry_run` reports every decision without changing the store.
    ///
    /// Also ensures the default project and its backward-compat default team
    /// exist (skipped in dry run).
    pub fn migrate_legacy_flat_opt(
        &self,
        project: &str,
        opts: MigrateOptions,
    ) -> Result<MigrationReport> {
        let mut report = MigrationReport {
            dry_run: opts.dry_run,
            ..Default::default()
        };
        let project_missing = Project::load(self, project).is_err();
        if !opts.dry_run && project_missing {
            self.create_project(project, "Default corpus project", "cdk-regtest")?;
        }
        for category in CATEGORIES {
            let legacy_dir = self.root.join(category);
            if !legacy_dir.is_dir() {
                continue;
            }
            let mut entries: Vec<PathBuf> = fs::read_dir(&legacy_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();
            entries.sort();
            if entries.is_empty() {
                // Nothing to verify; the dir may go. Dry runs never mutate.
                if !opts.dry_run {
                    fs::remove_dir(&legacy_dir).ok();
                    report.removed_categories.push(category.to_string());
                }
                continue;
            }
            let dest_dir = self.project_corpus_dir(project).join(category);
            if !opts.dry_run {
                fs::create_dir_all(&dest_dir)?;
            }
            // The gate: only this dir may be dropped if EVERY entry it held
            // moved and checksum-verified (or was left in place as skipped).
            let mut category_verified = true;
            for entry in entries {
                let name = entry
                    .file_name()
                    .ok_or_else(|| Error::Store("bad entry name".into()))?;
                let target = dest_dir.join(&name);
                if target.exists() {
                    // Never overwrite: leave the source entry in place.
                    report.skipped.push(entry);
                    continue;
                }
                if opts.dry_run {
                    report.would_move.push(entry);
                    continue;
                }
                let expected = checksum(&entry)?;
                fs::rename(&entry, &target)?;
                let actual = checksum(&target)?;
                if actual != expected {
                    category_verified = false;
                    report.unverified.push(target);
                } else {
                    report.moved.push(target);
                }
            }
            if !opts.dry_run
                && category_verified
                && fs::read_dir(&legacy_dir)?.next().is_none()
            {
                fs::remove_dir(&legacy_dir).ok();
                report.removed_categories.push(category.to_string());
            }
        }
        // Backward-compat unscoped scope: the default team on the default
        // project, instantiated from the two core agent templates.
        if !opts.dry_run
            && !self
                .team_dir(project, DEFAULT_TEAM_SLUG)
                .join("team.yaml")
                .is_file()
        {
            self.create_team(
                project,
                DEFAULT_TEAM_SLUG,
                DEFAULT_TEAM_SLUG,
                core_agent_instances(),
                None,
            )?;
        }
        Ok(report)
    }
}

// -------------------------------------------------------------------------
// Promotion (team corpus -> project corpus)
// -------------------------------------------------------------------------

/// The result of a successful promotion.
#[derive(Debug, Clone)]
pub struct Promoted {
    /// Destination path in the project-global corpus.
    pub entry: PathBuf,
    /// The sensitivity class recorded on the promoted entry.
    pub sensitivity: Sensitivity,
    /// Provenance recorded in the entry's frontmatter.
    pub provenance: String,
}

impl Store {
    /// Promote an entry from a team corpus into the project-global corpus.
    ///
    /// Sensitivity is read from the entry's frontmatter (default: findings
    /// embargoed, everything else internal, per roadmap data-security).
    /// Embargoed entries require `confirm` — the explicit operator act that
    /// lets a crown-jewel artifact leave the team scope. The promoted file's
    /// frontmatter gains `sensitivity:` and `promoted_from:` (the team
    /// provenance); everything else is copied verbatim.
    pub fn promote_entry(
        &self,
        project: &str,
        team: &str,
        category: &str,
        entry: &str,
        confirm: bool,
    ) -> Result<Promoted> {
        if !CATEGORIES.contains(&category) {
            return Err(Error::Store(format!("unknown category: {category}")));
        }
        let spec = TeamSpec::load(self, project, team)?;
        let source = self.team_corpus_dir(project, team).join(category).join(entry);
        if !is_entry(&source) {
            return Err(Error::Store(format!(
                "no such entry: {project}/{team} {category}/{entry}"
            )));
        }
        let sensitivity = read_entry_sensitivity(&source, category)?;
        if sensitivity.promotion_requires_confirm() && !confirm {
            return Err(Error::Store(format!(
                "refusing to promote embargoed entry {category}/{entry}: \
                 pass confirm: true to lift it out of the {project}/{team} scope"
            )));
        }
        let dest = self.project_corpus_dir(project).join(category).join(entry);
        if dest.exists() {
            return Err(Error::Store(format!("already in project corpus: {category}/{entry}")));
        }
        fs::create_dir_all(
            dest.parent()
                .ok_or_else(|| Error::Store("bad destination path".into()))?,
        )?;
        copy_entry(&source, &dest)?;
        let fixed = dest.is_dir()
            .then(|| dest.join("attack.md"))
            .unwrap_or_else(|| dest.clone());
        let provenance = spec.provenance(project, team);
        let text = fs::read_to_string(&fixed)?;
        let updated = frontmatter::insert_into_frontmatter(
            &text,
            &[("sensitivity", sensitivity.as_str()), ("promoted_from", &provenance)],
        )?;
        fs::write(&fixed, updated)?;
        Ok(Promoted {
            entry: dest,
            sensitivity,
            provenance,
        })
    }
}

/// True if `path` is a corpus entry: a file, or a directory containing an
/// attack.md (attacks are directories).
fn is_entry(path: &Path) -> bool {
    if path.is_file() {
        return true;
    }
    path.is_dir() && path.join("attack.md").is_file()
}

/// Read an entry's sensitivity from its frontmatter (`attack.md` for attack
/// dirs), defaulting by category.
fn read_entry_sensitivity(entry: &Path, category: &str) -> Result<Sensitivity> {
    let probe = if entry.is_dir() { entry.join("attack.md") } else { entry.to_path_buf() };
    let text = fs::read_to_string(&probe)?;
    let (fm, _) = frontmatter::split(&text)?;
    match fm {
        Some(fm) => Sensitivity::from_frontmatter(&fm, category),
        None => Ok(Sensitivity::default_for_category(category)),
    }
}

/// Copy an entry (file or attack dir) preserving permissions.
fn copy_entry(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        copy_tree(src, dst)
    } else {
        fs::copy(src, dst)?;
        Ok(())
    }
}

/// A content hash over an ENTIRE entry: a file's bytes, or — for attack
/// directories — every file beneath it (sorted, name-length-prefixed so
/// renames cannot paste different files onto the same checksum). This is the
/// checksum the migration verifies before and after each move.
pub fn checksum(path: &Path) -> Result<String> {
    if !path.is_dir() {
        return Ok(fnv1a_hex(&fs::read(path)?));
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(path, path, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (rel, abs) in &files {
        for b in rel.len().to_be_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for b in rel.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let bytes = fs::read(abs)?;
        for b in &bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Ok(format!("{hash:016x}"))
}

/// Collect `(relative_path, absolute_path)` for every file under `root`.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let abs = entry.path();
        let rel = abs
            .strip_prefix(root)
            .expect("collect_files dir is under root")
            .to_string_lossy()
            .into_owned();
        if abs.is_dir() {
            collect_files(root, &abs, out)?;
        } else {
            out.push((rel, abs));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("corpus-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn validate_slug_rejects_bad_names() {
        for bad in ["", "..", "a/b", "Upper", "under_score", "-lead", "trail-", "a b"] {
            assert!(validate_slug(bad).is_err(), "should reject {bad:?}");
        }
        for good in ["default", "cdk-regtest", "red-alpha-2"] {
            assert!(validate_slug(good).is_ok(), "should accept {good:?}");
        }
    }

    #[test]
    fn project_crud() {
        let store = tmp_store("proj");
        let project = store.create_project("cdk-a", "CDK team A", "cdk-regtest").unwrap();
        assert_eq!(project.plugin, "cdk-regtest");
        assert!(store.project_dir("cdk-a").join("project.yaml").is_file());
        assert!(store.project_corpus_dir("cdk-a").join("findings").is_dir());
        // duplicate create fails
        assert!(store.create_project("cdk-a", "x", "y").is_err());
        // list
        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        // clone without corpus
        store.clone_project("cdk-a", "cdk-b", None, false).unwrap();
        let b = Project::load(&store, "cdk-b").unwrap();
        assert_eq!(b.cloned_from.as_deref(), Some("cdk-a"));
        // delete
        store.delete_project("cdk-b").unwrap();
        assert!(Project::load(&store, "cdk-b").is_err());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn migrate_relocates_bytes_verbatim() {
        let store = tmp_store("migrate");
        // legacy flat layout with bytes that matter
        write(&store.root().join("findings/1786000000-theft.md"), "---\ntitle: T\nseverity: high\n---\n\nsecret bytes\n");
        write(&store.root().join("techniques/quote-race.md"), "# quote race\nwith leading em-dash — and trailing.\n");
        write(&store.root().join("attacks/quote-id-front-run/attack.md"), "# Attack\n\nbody\n");
        write(&store.root().join("attacks/quote-id-front-run/run.sh"), "#!/bin/sh\necho hi\n");
        write(&store.root().join("runs/1700000000-op-run.log"), "# run\npayload\n");
        write(&store.root().join("hypotheses/lead.md"), "---\nstatus: open\n---\n\nbody\n");

        // Snapshot the legacy bytes (checksums before migration).
        let mut before = std::collections::HashMap::new();
        for (cat, name) in [
            ("findings", "1786000000-theft.md"),
            ("techniques", "quote-race.md"),
            ("attacks", "quote-id-front-run/attack.md"),
            ("attacks", "quote-id-front-run/run.sh"),
            ("runs", "1700000000-op-run.log"),
            ("hypotheses", "lead.md"),
        ] {
            let bytes =
                fs::read(store.root().join(cat).join(name)).unwrap();
            before.insert(format!("{cat}/{name}"), bytes);
        }

        let report = store.migrate_legacy_flat(DEFAULT_PROJECT_SLUG).unwrap();
        assert_eq!(report.moved.len(), 5);
        assert!(report.unverified.is_empty(), "no unverified moves: {:?}", report.unverified);
        assert_eq!(report.removed_categories.len(), 5, "all legacy dirs dropped once verified");
        assert!(!report.dry_run);
        // every legacy dir is gone
        for cat in CATEGORIES {
            assert!(!store.root().join(cat).exists(), "{cat} should be empty/gone");
        }
        // byte-identical at scoped destination
        let target = store.project_corpus_dir(DEFAULT_PROJECT_SLUG);
        for (rel, expected) in &before {
            let path = target.join(rel);
            assert!(path.is_file(), "missing {}", path.display());
            assert_eq!(&fs::read(&path).unwrap(), expected, "byte identity of {rel}");
        }
        // default team exists (backward-compat unscoped scope)
        let spec = TeamSpec::load(&store, DEFAULT_PROJECT_SLUG, DEFAULT_TEAM_SLUG).unwrap();
        assert_eq!(spec.corpus_generation, 0);
        // idempotent second run
        let second = store.migrate_legacy_flat(DEFAULT_PROJECT_SLUG).unwrap();
        assert_eq!(second.moved.len(), 0);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn migrate_skips_prepopulated_destination_never_overwrites() {
        let store = tmp_store("migrate-skip");
        // Legacy flat file.
        write(
            &store.root().join("findings/1786000000-theft.md"),
            "---\ntitle: legacy\n---\n\nlegacy bytes\n",
        );
        // Pre-populated destination: the project corpus already holds a file
        // under the same name with DIFFERENT bytes.
        store.create_project("default", "Default corpus project", "cdk-regtest").unwrap();
        write(
            &store
                .project_corpus_dir("default")
                .join("findings/1786000000-theft.md"),
            "---\ntitle: existing\n---\n\nexisting bytes, never to be clobbered\n",
        );
        let dest_before = fs::read(
            store
                .project_corpus_dir("default")
                .join("findings/1786000000-theft.md"),
        )
        .unwrap();

        let report = store.migrate_legacy_flat("default").unwrap();
        assert_eq!(report.moved.len(), 0);
        assert_eq!(report.skipped.len(), 1, "legacy entry reported as skipped");
        // Destination untouched (never overwritten).
        assert_eq!(
            fs::read(
                store
                    .project_corpus_dir("default")
                    .join("findings/1786000000-theft.md")
            )
            .unwrap(),
            dest_before,
            "pre-populated destination is byte-identical after migrate"
        );
        // Legacy source left in place, and the legacy dir NOT removed because
        // the entry it held was skipped, not verified-moved.
        assert!(
            store.root().join("findings/1786000000-theft.md").is_file(),
            "skipped legacy entry stays where it was"
        );
        assert!(
            !report.removed_categories.contains(&"findings".to_string()),
            "legacy dir kept while its entry is skipped"
        );
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn migrate_dry_run_changes_nothing() {
        let store = tmp_store("migrate-dry");
        write(&store.root().join("techniques/race.md"), "# race\n");
        write(&store.root().join("runs/1-op.log"), "# run\n");
        let before: Vec<PathBuf> = walk_entries(&store.root().join("techniques"));

        let report = store
            .migrate_legacy_flat_opt("default", MigrateOptions { dry_run: true })
            .unwrap();
        assert!(report.dry_run);
        assert_eq!(report.would_move.len(), 2);
        assert!(report.moved.is_empty() && report.unverified.is_empty());
        // Nothing was created or moved: legacy files still in place, and no
        // project/dest tree appeared.
        assert!(store.root().join("techniques/race.md").is_file());
        assert!(store.root().join("runs/1-op.log").is_file());
        assert!(!store.project_dir("default").exists(), "no project created by dry run");
        assert_eq!(walk_entries(&store.root().join("techniques")), before);
        let _ = fs::remove_dir_all(store.root());
    }

    fn walk_entries(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.filter_map(|e| e.ok()) {
                out.push(e.path());
            }
        }
        out.sort();
        out
    }

    #[test]
    fn team_roundtrip_clone_and_wipe() {
        let store = tmp_store("team");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let team = store
            .create_team(
                "p",
                "red",
                "Red team",
                core_agent_instances(),
                None,
            )
            .unwrap();
        assert_eq!(team.corpus_generation, 0);
        write(
            &store.team_corpus_dir("p", "red").join("techniques/race.md"),
            "---\nname: race\n---\n\nbody\n",
        );
        write(
            &store.team_corpus_dir("p", "red").join("findings/1-theft.md"),
            "---\nseverity: high\n---\n\nbody\n",
        );
        let prov = team.provenance("p", "red");
        assert!(prov.starts_with("p/red@"));

        // clone: deep copy, generation snapshot
        store.clone_team("p", "red", "blue").unwrap();
        let blue = TeamSpec::load(&store, "p", "blue").unwrap();
        assert_eq!(blue.corpus_generation, 0);
        assert!(store.team_corpus_dir("p", "blue").join("techniques/race.md").is_file());
        assert!(store.team_corpus_dir("p", "blue").join("findings/1-theft.md").is_file());

        // wipe the original: generation bumps, corpus gone, clone untouched
        let red = store.wipe_team_corpus("p", "red").unwrap();
        assert_eq!(red.corpus_generation, 1);
        assert!(!store.team_corpus_dir("p", "red").join("techniques/race.md").exists());
        assert!(store.team_corpus_dir("p", "blue").join("findings/1-theft.md").is_file());
        assert_eq!(TeamSpec::load(&store, "p", "blue").unwrap().corpus_generation, 0);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn promote_gates_on_sensitivity() {
        let store = tmp_store("promote");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_team("p", "t", "T", core_agent_instances(), None)
            .unwrap();
        // internal technique promotes freely
        write(
            &store.team_corpus_dir("p", "t").join("techniques/race.md"),
            "---\nname: race\nstatus: fired\nrun_log: 1.log\n---\n\nbody\n",
        );
        let promoted = store
            .promote_entry("p", "t", "techniques", "race.md", false)
            .unwrap();
        let dest_text = fs::read_to_string(&promoted.entry).unwrap();
        assert!(dest_text.contains("sensitivity: internal"));
        assert!(dest_text.contains(&format!("promoted_from: {}", promoted.provenance)));
        assert_eq!(promoted.sensitivity, Sensitivity::Internal);

        // embargoed finding refuses without confirm
        write(
            &store.team_corpus_dir("p", "t").join("findings/1-theft.md"),
            "---\nseverity: high\nsensitivity: embargoed\n---\n\nbody\n",
        );
        let err = store
            .promote_entry("p", "t", "findings", "1-theft.md", false)
            .unwrap_err();
        assert!(err.to_string().contains("refusing to promote embargoed"));

        // confirm lifts it, provenance records the source scope
        let promoted = store
            .promote_entry("p", "t", "findings", "1-theft.md", true)
            .unwrap();
        let dest_text = fs::read_to_string(&promoted.entry).unwrap();
        assert!(dest_text.contains("sensitivity: embargoed"));
        assert!(dest_text.contains(&format!("promoted_from: {}", promoted.provenance)));
        assert_eq!(promoted.provenance, TeamSpec::load(&store, "p", "t").unwrap().provenance("p", "t"));
        let _ = fs::remove_dir_all(store.root());
    }
}