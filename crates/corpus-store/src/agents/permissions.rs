//! Role-derived permission binding and filesystem/delegation sealing.

use std::collections::{BTreeMap, BTreeSet};

use super::roles::PROJECT_MANAGEMENT_TOOLS;
use super::{AgentRole, CORPUS_TOOLS, LEGACY_CORPUS_TOOLS};
use crate::store::Store;

/// What a render binds an entry to, beyond the entry's own config.
pub(super) struct RenderCtx<'a> {
    /// The project the rendered agent is bound to.
    pub(super) project: &'a str,
    /// This entry's capability ceiling.
    pub(super) role: AgentRole,
    /// Every entry name the project declares — the delegation universe.
    /// A `task:` allow outside it is force-denied, so the artifact cannot
    /// point opencode at an agent the run dir does not contain.
    pub(super) known_entries: &'a BTreeSet<String>,
    /// The absolute data roots, denied by path. The run cwd's relative
    /// patterns describe only what the run dir links; these close the
    /// absolute route to everything it doesn't.
    pub(super) roots: DataRoots,
}

/// One permission decision. Ordered by tightness, so a merge that takes
/// the MAXIMUM moves a ceiling in the only direction a stored document is
/// allowed to move it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    Allow,
    Ask,
    Deny,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    /// Read a stored value. `None` if it is not a string at all; anything
    /// that IS a string but not one of opencode's three words reads as
    /// `Deny` — a permission we cannot interpret is not one we can honour.
    /// The predecessor copied such a string into the artifact verbatim and
    /// left opencode to decide what `"maybe"` meant.
    fn parse(value: &serde_json::Value) -> Option<Self> {
        match value.as_str()? {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            _ => Some(Self::Deny),
        }
    }
}

/// A glob rule family (`read`/`edit`/`write`/`task`): pattern -> action.
///
/// Sorted, because `canonical_json` sorts the block before it is written
/// and opencode evaluates last-match-wins over that order — so a narrow
/// rule only beats the broad one it refines when it sorts AFTER it. Every
/// pattern injected below extends the prefix of the rule it must beat,
/// which is what makes that hold.
type Rules = BTreeMap<String, Action>;

fn rules<const N: usize>(entries: [(&str, Action); N]) -> Rules {
    entries
        .into_iter()
        .map(|(pattern, action)| (pattern.to_string(), action))
        .collect()
}

/// The rendered permission block as a VALUE: computed once from the role
/// and the stored document, then serialized.
///
/// The predecessor derived the same block by mutating a single string-keyed
/// JSON map through nine stages. Every failure that design actually shipped
/// was a rule that silently did not land, and each was invisible in the
/// same way — nothing was wrong with the stage, the stage simply had
/// nowhere to write:
///
/// - a scalar `read: "allow"` is not a map, so the red lines injected into
///   `read` went into a value that discarded them;
/// - agents built from a role carry no permission block at all, so entries
///   whose rules were only ever DEFAULTED rendered with no path rules;
/// - `bash` was defaulted where it meant to be forced, so a stored
///   `"bash": "allow"` survived every render — against a module doc that
///   had claimed the opposite since roles landed.
///
/// A field cannot be absent. That is the whole reason this is a struct:
/// the compiler now asks the question each of those bugs answered wrongly.
struct Policy {
    read: Rules,
    edit: Rules,
    write: Rules,
    /// Delegation, confined to entries the project actually declares.
    /// `render_project_agents` already refuses a dangling name outright;
    /// this keeps the ARTIFACT safe on any path that renders without it.
    task: Rules,
    bash: Action,
    webfetch: Action,
    websearch: Action,
    /// Reaching outside the run dir at all. The run cwd exposes exactly
    /// one project by construction; this is the switch deciding whether
    /// that construction can be stepped around.
    external_directory: Action,
    /// The `corpus_*` switches: 10 active sandbox/corpus tools, one legacy
    /// alias, and 29 management ones,
    /// every one written explicitly so the artifact never leans on
    /// omission-means-allow. Three come out `corpus_corpus_*` because the
    /// run config names the MCP server `corpus`.
    tools: BTreeMap<String, Action>,
    /// Keys this type does not model — opencode's own `glob`, `grep`,
    /// `list`, and whatever a later version adds. Carried through
    /// unchanged: the render binds and tightens, and silently dropping a
    /// setting it fails to recognize is neither.
    passthrough: serde_json::Map<String, serde_json::Value>,
}

/// Bind a permission document to a concrete project AND a role at render
/// time. The rendered artifact — not the stored JSON — is what opencode
/// obeys, so deriving here means role and document can never contradict in
/// the dangerous direction, however the stored block was edited.
pub(super) fn bind_permission(
    permission: &serde_json::Value,
    ctx: &RenderCtx<'_>,
) -> serde_json::Value {
    Policy::build(permission, ctx).into_json()
}

impl Policy {
    fn build(permission: &serde_json::Value, ctx: &RenderCtx<'_>) -> Self {
        let role = ctx.role;
        // The project rewrite runs ONCE, over the stored document only.
        // Everything injected below is written with the concrete project
        // already in it — except two deliberate `store/projects/*`
        // wildcards, which mean all projects and must survive as such.
        let mut stored = match bind_project(permission, ctx.project) {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };

        let corpus = format!("store/projects/{}/corpus/**", ctx.project);
        let runs = format!("store/projects/{}/corpus/runs/**", ctx.project);
        // `runs/` sits inside the corpus but is not an agent's to change:
        // those transcripts are what technique cards cite by name, what the
        // cost report counts, and the only provenance a mission leaves.
        let mutable_default = || {
            rules([
                ("*", Action::Deny),
                (corpus.as_str(), Action::Allow),
                (runs.as_str(), Action::Deny),
            ])
        };
        let mut read =
            take_rules(&mut stored, "read").unwrap_or_else(|| rules([("*", Action::Allow)]));
        let mut edit = take_rules(&mut stored, "edit").unwrap_or_else(mutable_default);
        let mut write = take_rules(&mut stored, "write").unwrap_or_else(mutable_default);
        seal_readable(&mut read, ctx);
        seal_mutable(&mut edit, ctx);
        seal_mutable(&mut write, ctx);

        let mut task = take_rules(&mut stored, "task").unwrap_or_default();
        // Omission would inherit opencode's default and let a dangling name
        // resolve against config discovered outside the run dir — the leak
        // that sent one project's scout at another's corpus.
        task.entry("*".to_string()).or_insert(Action::Deny);
        for (name, action) in task.iter_mut() {
            if name != "*" && !ctx.known_entries.contains(name.as_str()) {
                *action = Action::Deny;
            }
        }

        let mut tools = BTreeMap::new();
        let stored_probe = take_action(&mut stored, "corpus_probe_save");
        let stored_attack = take_action(&mut stored, "corpus_attack_save");
        let stored_probe_capability = match (stored_probe, stored_attack) {
            (Some(current), Some(legacy)) => Some(current.max(legacy)),
            (current, legacy) => current.or(legacy),
        };
        for tool in CORPUS_TOOLS {
            let stored = if tool == "corpus_probe_save" {
                stored_probe_capability
            } else {
                take_action(&mut stored, tool)
            };
            tools.insert(tool.to_string(), ceiling(role.allows(tool), stored));
        }
        for tool in LEGACY_CORPUS_TOOLS {
            tools.insert(
                tool.to_string(),
                ceiling(role.allows(tool), stored_probe_capability),
            );
        }
        let granted = role.admin_tools();
        for tool in PROJECT_MANAGEMENT_TOOLS {
            let key = format!("corpus_{tool}");
            let stored = take_action(&mut stored, &key);
            tools.insert(key, ceiling(granted.contains(&tool), stored));
        }

        // Web is opencode-enforced; the role decides whether to offer it.
        // Written either way — a rendered file must never depend on
        // opencode's default for a capability the role has an opinion on.
        let offered = match role.grants_web() {
            true => Action::Allow,
            false => Action::Deny,
        };
        let web = |stored: Option<Action>| match stored {
            Some(Action::Deny) => Action::Deny,
            _ => offered,
        };
        let webfetch = web(take_action(&mut stored, "webfetch"));
        let websearch = web(take_action(&mut stored, "websearch"));

        // A host shell defeats the whole gate for any role the server
        // restricts: it re-execs corpus-mcp with a forged
        // `CORPUS_OPENCODE_AGENT`, or edits the sidecar through the `store`
        // link the run dir provides.
        let stored_bash = take_action(&mut stored, "bash");
        let bash = match role.shell_would_defeat_gate() {
            true => Action::Deny,
            false => stored_bash.unwrap_or(Action::Deny),
        };

        // Written for EVERY role, `super` included. The exemption that used
        // to skip the key for `super` was meant to let the unrestricted
        // role step outside — but an absent key hands the decision to
        // opencode's default, which denies. So the exemption delivered
        // nothing except an artifact that did not say what it meant.
        let _ = take_action(&mut stored, "external_directory");
        let external_directory = Action::Deny;

        Self {
            read,
            edit,
            write,
            task,
            bash,
            webfetch,
            websearch,
            external_directory,
            tools,
            passthrough: stored,
        }
    }

    fn into_json(self) -> serde_json::Value {
        let mut out = self.passthrough;
        for (key, family) in [
            ("read", self.read),
            ("edit", self.edit),
            ("write", self.write),
            ("task", self.task),
        ] {
            let rules = family
                .into_iter()
                .map(|(pattern, action)| (pattern, scalar(action)))
                .collect();
            out.insert(key.to_string(), serde_json::Value::Object(rules));
        }
        for (key, action) in [
            ("bash", self.bash),
            ("webfetch", self.webfetch),
            ("websearch", self.websearch),
            ("external_directory", self.external_directory),
        ] {
            out.insert(key.to_string(), scalar(action));
        }
        for (tool, action) in self.tools {
            out.insert(tool, scalar(action));
        }
        serde_json::Value::Object(out)
    }
}

fn scalar(action: Action) -> serde_json::Value {
    serde_json::Value::String(action.as_str().to_string())
}

/// The ceiling merge. Outside the role's grant a tool is denied whatever
/// the document said; inside it, a stored `deny`/`ask` TIGHTENS and is
/// kept, so hand-tightening an agent works and hand-widening one does not.
fn ceiling(granted: bool, stored: Option<Action>) -> Action {
    match granted {
        false => Action::Deny,
        true => stored.unwrap_or(Action::Allow),
    }
}

/// Pull a rule family out of the stored document, normalizing a bare
/// scalar to `{"*": scalar}` so the red lines have somewhere to land.
fn take_rules(stored: &mut serde_json::Map<String, serde_json::Value>, key: &str) -> Option<Rules> {
    let value = stored.remove(key)?;
    let mut out = Rules::new();
    match &value {
        serde_json::Value::Object(map) => {
            for (pattern, action) in map {
                if let Some(action) = Action::parse(action) {
                    out.insert(pattern.clone(), action);
                }
            }
        }
        other => {
            if let Some(action) = Action::parse(other) {
                out.insert("*".to_string(), action);
            }
        }
    }
    Some(out)
}

fn take_action(
    stored: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<Action> {
    Action::parse(&stored.remove(key)?)
}

/// Emit a string as a YAML scalar that survives a round trip.
///
/// The frontmatter used to interpolate these raw, so any description
/// containing `": "` produced a file whose frontmatter does not parse —
/// and opencode reads the permission block out of that frontmatter, so the
/// whole role gate on the opencode side went with it. The `super` role's
/// own description tripped this, silently, for every agent rendered under
/// it. Quoting is delegated to the YAML adapter rather than hand-rolled: a value
/// that needs no quotes still renders bare, so the artifacts of every
/// colon-free description are byte-identical to before.
pub(super) fn yaml_scalar(value: &str) -> String {
    crate::yaml::to_string(value)
        .map(|s| s.trim_end().to_string())
        .unwrap_or_else(|_| format!("{value:?}"))
}

/// Rewrite `store/projects/*` to the concrete project in every key, at
/// every depth of the stored document.
///
/// Applied to the STORED document only. The wildcards this render injects
/// itself (`store/projects/*` in `read`, `store/projects/*/agents/**` in
/// `edit`/`write`) mean every project and must stay wildcards — which is
/// why binding runs first and injection second, rather than the two being
/// interleaved as they were when one function did both.
fn bind_project(value: &serde_json::Value, project: &str) -> serde_json::Value {
    let serde_json::Value::Object(map) = value else {
        return value.clone();
    };
    let bound = format!("store/projects/{project}");
    serde_json::Value::Object(
        map.iter()
            .map(|(key, value)| {
                (
                    key.replace("store/projects/*", &bound),
                    bind_project(value, project),
                )
            })
            .collect(),
    )
}

/// The red lines on READING, injected rather than trusted.
fn seal_readable(rules: &mut Rules, ctx: &RenderCtx<'_>) {
    // Contamination rule: the answer key and harness internals stay
    // unreadable even if edited out of a config.
    for red in ["benchmarks/**", "plugins/**"] {
        rules.entry(red.to_string()).or_insert(Action::Deny);
    }
    // The project boundary, RELATIVE — what the run cwd exposes. Narrowed
    // to the corpus and mission records: the project's `agents/` holds the
    // sidecars this gate trusts and `var/` its chat scope, neither of which
    // is research material. Only applied to a document that opened
    // everything and drew no boundary of its own.
    let opens_everything = rules.get("*") == Some(&Action::Allow);
    let has_boundary = rules.keys().any(|k| k.starts_with("store/projects/"));
    if opens_everything && !has_boundary {
        rules.insert("store/projects/*".to_string(), Action::Deny);
        for allowed in ["corpus", "missions"] {
            rules.insert(
                format!("store/projects/{}/{allowed}/**", ctx.project),
                Action::Allow,
            );
        }
    }
    seal_data_roots(rules, ctx, false);
}

/// The red lines on WRITING. The agent tree holds the role sidecars this
/// gate trusts, and the run cwd links the project — no agent writes there.
fn seal_mutable(rules: &mut Rules, ctx: &RenderCtx<'_>) {
    rules.insert("store/projects/*/agents/**".to_string(), Action::Deny);
    seal_data_roots(rules, ctx, true);
}

/// Deny the data roots by ABSOLUTE path, then re-allow exactly one
/// project's corpus.
///
/// The relative patterns above describe the run cwd and say nothing about
/// `/Users/…/.corpus/store/...`; the run dir links only one project, so an
/// absolute path is the one route left to name another's corpus.
///
/// Ordering matters and is not incidental: the block is sorted before it is
/// written and opencode evaluates last-match-wins, so the narrow allow must
/// sort AFTER the broad deny. `<data>/**` < `<data>/store/projects/<p>/…`
/// holds because the allow extends the deny's prefix — every allow emitted
/// here must keep that property.
fn seal_data_roots(rules: &mut Rules, ctx: &RenderCtx<'_>, mutating: bool) {
    let roots = &ctx.roots;
    if !roots.data.is_empty() {
        let data = roots.data.trim_end_matches('/');
        // Denied by name rather than by denying the whole data root: the
        // agent's own run dir lives under that root too, and a blanket deny
        // would take its cwd (and the `sources` link) with it if opencode
        // resolves paths before matching.
        //
        // `chat` is the operator's notes across every project. `audit` and
        // `refusals` are the records OF this agent — deny-by-default
        // already covers them for writes, but a subject editing its own
        // entry is the one failure those logs cannot tolerate, so it is
        // stated rather than inferred. Reading them is denied too: between
        // them they are a map of every gate and the exact wording that
        // trips it.
        for log in ["audit", "chat", "refusals"] {
            rules.insert(format!("{data}/var/{log}/**"), Action::Deny);
        }
    }
    if roots.store.is_empty() {
        return;
    }
    let store = roots.store.trim_end_matches('/');
    rules.insert(format!("{store}/**"), Action::Deny);
    rules.insert(
        format!("{store}/projects/{}/corpus/**", ctx.project),
        Action::Allow,
    );
    // The absolute half of the `runs/` rule. Reading a transcript is fine —
    // an agent may want its own — but changing one is not.
    if mutating {
        rules.insert(
            format!("{store}/projects/{}/corpus/runs/**", ctx.project),
            Action::Deny,
        );
    }
}

/// The absolute roots a render denies by path. Held as strings because
/// they only ever become permission-rule keys.
#[derive(Debug, Clone, Default)]
pub(super) struct DataRoots {
    /// Everything the operator owns (`~/.corpus`) — chat scopes and run
    /// dirs included, not just the store.
    data: String,
    /// The store root, whose one allowed project subtree is re-opened.
    store: String,
}

impl DataRoots {
    /// Derived from the STORE, never from the environment: a render must
    /// produce the same bytes for the same store regardless of what
    /// `CORPUS_STORE` happens to say in this process. The store's parent
    /// is denied too — that is where `var/run` and `var/chat` live, so the
    /// deny covers run dirs and management-chat transcripts as well.
    pub(super) fn for_store(store: &Store) -> Self {
        Self {
            data: store
                .root()
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            store: store.root().to_string_lossy().into_owned(),
        }
    }
}

/// Recursively sort object keys so rendered bytes are identical no matter
/// how feature unification ordered serde_json's map (`preserve_order`
/// leaks in via sibling deps; without this, which binary rendered last
/// decides the byte order and the checked-in agent files flip-flop).
pub(super) fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.clone(), canonical_json(v)))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_json).collect())
        }
        other => other.clone(),
    }
}
