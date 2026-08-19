# AGENTS.md — corpus

corpus is a local-first vulnerability research platform: a team of AI
researchers investigate codebases at pinned commits, hunt for
vulnerabilities in a sandboxed environment, and compile findings into a
verifiable knowledge base (the corpus store). Everything project-specific —
how to boot a regtest network, what invariants must hold, which tools an
attacker gets — is an environment *plugin*, so corpus can be pointed at any
open-source project with a reproducible test environment.

## Workspace layout

```
AGENTS.md            this file (read first)
PLUGINS.md           operator install/upgrade and plugin authoring contract
Cargo.toml           workspace root
crates/
  corpus-store/      filesystem data model, finding severity/discovery,
                     agent policy/rendering, audit/refusal, and root paths
  corpus-observe/    read-only host observation: installed plugin manifests,
                     run activity, pin catalogs, and model discovery
  corpus-core/       compatibility facade plus plugin protocol, source
                     resolution/materialization, and run launch machinery
  corpus-admin/      host/scoped management catalog and handlers, depending
                     directly on corpus-store + corpus-observe
  corpus-admin-mcp/  dedicated host-side administration MCP executable
  corpus-mcp/        MCP server exposing the corpus tools to agents
  corpus-cli/        headless CLI: run / plugin / models / store administration
  corpus-app/        egui desktop app (corpus-app), the operator UI
benchmarks/
  models.yaml        model registry (identity + metadata; scores live in
                     benchmarks/results/<model>/*.yaml)
  forensic/          historical-bug forensic suite (CDK-BENCH-XXXX.yaml)
  results/           per-model benchmark score results
scripts/goose-recipes/ optional Goose CLI fallback recipes. Script-only;
                     corpus-app uses the embedded chat runtime.
docs/                (gone — folded into dev/; see below)
dev/                 everything uncommitted & machine-local: architecture,
                     decisions, research, alpha-1, plus ACTIVE plans
                     (roadmap-plan, data-model-plan, app-flow-plan,
                     app-parity-spec, mission-view-plan)
                     and the demo poster. Git-ignored: may be absent on
                     a fresh clone.
.opencode/           opencode SKILLS only. No agent files and no
                     opencode.json: agents are generated per project into
                     that project's run dir, and the MCP config is written
                     there too. Anything project-bound checked in here is
                     discoverable by every run — that is how one project's
                     agent came to read another's corpus.
```

## Where the data lives

```
~/.corpus/                        CORPUS_HOME — everything the operator produces
  store/projects/<slug>/          CORPUS_STORE
    project.yaml  corpus/  agents/  missions/
  var/run/<slug>/                 the opencode run cwd, per project
  var/chat/<slug>/                goose management-chat scope
  var/audit/<slug>.jsonl          curator acts (`corpus audit`)
  var/refusals/<slug>.jsonl       calls the server turned away (`corpus refusals`)
  app.yaml                        app prefs
```

A run dir is provisioned per launch and exposes EXACTLY one project:

```
~/.corpus/var/run/<slug>/                    outside any git repo
  store/projects/<slug> -> ~/.corpus/store/projects/<slug>   only this one
  sources               -> <resources>/sources
  .opencode/opencode.json                    generated; carries CORPUS_PROJECT
  .opencode/agent/*.md                       rendered from the project's agents
```

The run dir is per PROJECT and shared by every mission in it, so everything
in it must be a project-level fact. Per-RUN identity — `CORPUS_OPENCODE_AGENT`,
`CORPUS_RUN_LOG`, `CORPUS_SOURCE_PINS` — travels as environment on the tmux
session instead, and reaches the agent through `target_info`. That split is
what lets several missions run at once: `render_project_agents` takes no
launch state, so a second mission's launch rewrites the first's agent files
with identical bytes. The renderer used to bake each launch's literal
`sources/<name>/<sha>/` trees into those files, which meant launching one
mission silently repointed a live one at another mission's revisions.

That is the project boundary. The permission globs in a rendered agent are
the second line of defence, not the only one: another project's corpus is
absent from the namespace rather than deny-listed, and `benchmarks/` (the
answer key) and `plugins/` are unreachable rather than forbidden. Because
the run cwd sits outside the repo, opencode's own upward config discovery
cannot reach this repo's `.opencode/` either.

The **resource root** (`CORPUS_RESOURCES`, else found from the running
executable — the directory holding shipped assets such as
`benchmarks/models.yaml`) is the
other half: shipped, read-only, replaced by an upgrade. `corpus-store/paths.rs`
owns both roots. `store.root().parent()` is NOT the repo root and must never
be used as one again.

## Build / test / run commands

```bash
cargo build -p corpus-store -p corpus-observe -p corpus-core -p corpus-mcp -p corpus-admin-mcp -p corpus-cli
cargo build -p corpus-app                                # the egui app
cargo test -p corpus-store -p corpus-observe -p corpus-core -p corpus-mcp -p corpus-admin -p corpus-admin-mcp
cargo test -p corpus-app --bin corpus-app                 # app + chat/team injection probes
scripts/check-dependency-policy                          # admin artifact boundary
cargo build --workspace                                   # everything
```

Chat/embedding build ergonomics (the `goose` git-dep):
- goose pins `rust-toolchain.toml = 1.96.1` — build/run with `+1.96.1`
  (`rustup install 1.96.1`) if rustc-version errors appear.
- This machine's global `~/.gitconfig` rewrites `https://github.com/` → SSH;
  the goose repo has NO SSH access here, so any fetch/build of `goose` needs
  `GIT_CONFIG_GLOBAL=/tmp/empty-gitconfig` (or
  `-c url.https://github.com/.insteadof=`) prepended.
- `goose` is optional behind `corpus-app`'s `chat-embed` feature (default ON).
  Headless/CI builds skip the whole goose tree with
  `cargo build -p corpus-app --no-default-features`.
- True cold compile of the goose tree is ~107s (1438 crates); incremental is
  sub-second. Consider `sccache` for repeated cold builds of the dep tree.

The external `cdk-regtest` and `nutshell-regtest` plugins are independently
versioned Docker environment bundles; neither is built by cargo. Install an
unpacked release bundle, then let Corpus fetch its manifest-pinned sources,
prepare tools/images/regtest, and verify it:

```bash
corpus plugin install ../corpus-plugin-cdk
corpus plugin setup cdk-regtest
corpus plugin doctor cdk-regtest

corpus plugin install ../corpus-plugin-nutshell
corpus plugin setup nutshell-regtest
corpus plugin doctor nutshell-regtest
```

Until the plugin repositories become public, the external-plugin compatibility
workflow reads pinned GitHub releases using the `CORPUS_PLUGIN_TOKEN`
repository secret. That token needs read-only Contents access to both private
plugin repositories; the workflow grants no write permission.

Environment is checked via `corpus plugin probe cdk-regtest`. The CLI:

```bash
# Discover plugins / probe the environment / make raw protocol calls
corpus plugin list
corpus plugin probe <name>
corpus plugin call <name> <method> [params-json]
# Run a mission on CORPUS_PROJECT (no default — an unscoped run refuses).
# The project's agents are materialized to the PROJECT's OWN
# .opencode/agent/ inside its run directory (~/.corpus/var/run/<p>/ —
# provisioned per launch, linking ONLY that project's store subtree plus
# sources/, with a generated opencode.json carrying CORPUS_PROJECT).
# opencode runs with that dir as cwd: each project owns its agent set AND
# its opencode session pool, and no other project exists in its namespace. The app
# launches the FULL opencode TUI in a
# DETACHED tmux session (corpus-<agent>-<ts>) and shows it in the
# EMBEDDED terminal pane (egui_term; the pane runs `tmux attach`
# in-process — no external-terminal popup): click the pane to steer,
# and the run survives attach/detach/app-close; a relaunched app lists
# live corpus-* sessions for in-pane re-attach. `corpus run` is the
# HEADLESS `opencode run` (automation): transcript .log in the project
# corpus runs/. Transcript of record for TUI runs: Stop (the ONE run
# teardown verb — the run menu is Stop / Rename… / Delete) exports
# the session to <epoch>-<agent>.json best-effort in the project corpus
# runs/; the live tail is tmux pipe-pane raw capture
# (ANSI-stripped), written into the SAME runs/ as <epoch>-<agent>.raw
# from the first output — a durable run log that survives app death,
# missing exports, and never-stopped sessions. The model is ALWAYS explicit (primary agent entry
# -> launch arg -> registry tool-use default; never opencode's ambient
# default — a launch with none refuses). App model fields are ONE shared
# picker (search + provider-grouped, views/model_picker.rs over
# corpus-core's model_list() = `opencode models --verbose`, TTL-cached,
# ↻ forces --refresh; no free-text model fields survive — opencode
# missing degrades the picker to free text + warning). Without tmux
# (>= 3.2a) the app degrades to the piped headless spawn (no attach);
# CORPUS_NO_TMUX=1 forces it.
# --research appends a researcher pass
corpus run <agent> [-m model] [--research] <mission...>
# Model registry
corpus models list
# Scoped store admin: projects, agents, missions
corpus project list|new|clone|delete|rebind|wipe <slug>
corpus agent list|new|clone|delete <project> ...   # new takes --role
corpus mission list|new|delete <project> ...
# Host-side corpus administration MCP server
corpus-admin-mcp                   # stdio MCP server; admin tools only
corpus audit <project> [--tail N]  # who changed this project (curator acts)
corpus refusals <project> [--tail N] [--gate G]  # what the server turned away
```

## corpus-admin MCP profile (dev/decisions.md — chat runtime closeout)

`corpus-admin-mcp` is the host-side operator artifact for natural-language
store administration via the GDK/goose chat. It sits OUTSIDE the research
trust domains (no sandbox, no oracles, no targets) and never runs missions
— it prepares them. It is UNSCOPED: 21 of its tools take a `project`
argument, so it reaches every project. That is why it is the operator's
profile and not an agent's. The tool group lives in `crates/corpus-admin`; the
dedicated binary starts store-only state and never spawns or probes a plugin.
It reads installed manifests, revision caches, run activity, and model catalogs
through `corpus-observe`. `crates/corpus-mcp/src/admin.rs` is only the
project-scoped adapter used by Curator/Super. The enforced boundary lives in
`scripts/check-dependency-policy`: the admin artifact may not depend on the
all-capabilities core facade or the research/app executables.

The same tools reach an IN-PROJECT agent through a different door: the
`curator` and `super` roles. There the ROLE decides the catalog (not an argv flag), and
`tools::dispatch` injects the project from the proven `CORPUS_PROJECT`
scope before any handler reads it — so neither role can name another
project. Project lifecycle tools and `agent_copy` are absent from both scoped
grant sets. `corpus_wipe` belongs to Super but not Curator. See "The curator
role" and the trust-domain table below.

Admin tools (thin over corpus-store and corpus-observe): `project_list/new/clone/delete/
rebind`, `agent_list/get/new/save/clone/delete` (`agent_new` builds the
opencode.json from structured fields — prefer it for creation; `agent_save`
only edits existing agents and runs the core validator, refusing invalid
documents with the validator's message), `mission_list/
get/new/delete/set_budget/set_pins`, `corpus_wipe`, `corpus_stats/list/read`,
`model_list` (discover opencode model ids for agent configs), plus
`entry_delete/move/write` for corpus curation.

- **Confirm-token gate (server-side, all five destructive ops):**
  `project_delete`, `agent_delete`, `mission_delete`, `corpus_wipe`, and
  `entry_delete` first
  return a DRY-RUN summary + a one-shot confirm token (hash of
  op+target+nonce, 60s TTL); the mutation only lands when the tool is
  re-called with that token, which is consumed (single-use). `corpus_wipe`
  without a token is a dry-run by construction.
- **`project_rebind` validates the plugin against the registry** before
  writing — a hallucinated/dangling plugin name is refused (chunk-0
  finding). "Budget" edits are per-MISSION (`mission_set_budget`), never
  per-agent.
- Tests: `crates/corpus-mcp/tests/admin_profile.rs` covers no-token
  dry-run, wrong/expired/single-use token, validator round-trip, rebind
  registry validation, and research-profile catalog exclusion;
  `crates/corpus-admin-mcp/tests/profile.rs` locks dedicated wire/catalog parity.

### GDK/goose chat wiring (chunk 1, local Ollama)

Goose config (`~/.config/goose/config.yaml`) declares the admin extension
and disables the shell:

```yaml
extensions:
  corpus-admin:
    type: stdio
    name: corpus-admin
    enabled: true
    cmd: /Users/admin/Sites/corpus/target/debug/corpus-admin-mcp
    args: []
    envs: {}
    timeout: 300
  developer:
    enabled: false   # "bash denied" — the shell is off by design
```

- Context size is explicit: `GOOSE_INPUT_LIMIT: 32768` at the **root** of
  config.yaml (goose's Ollama `num_ctx` override — provider-scoped is not
  the recognized knob) — without it Ollama silently truncates at 4096 and
  the session loops. Model stays explicit (`GOOSE_MODEL=qwen3.6:35b`).
- **Session storage is redirected into the project scope**, NOT goose's
  default `~/.local/share` session dir (chat transcripts carry finding
  material). goose has no session-db-only override; the supported
  mechanism is `GOOSE_PATH_ROOT` (reroutes the whole config/data/state
  tree), so the sessions DB lands at
  `~/.corpus/var/chat/<p>/data/sessions/`. ALWAYS launch the chat
  through the wrapper `scripts/goose-chat [goose args...]` (scope =
  `CORPUS_PROJECT`, default `default`) — it provisions the scope's own
  config.yaml (provider + corpus-admin + developer-off, `GOOSE_INPUT_LIMIT`
  at root) and execs goose with `GOOSE_PATH_ROOT` set. Example:
  `scripts/goose-chat run -n ops -t "<prompt>"`. A raw `goose run` outside
  the wrapper leaks sessions to the default dir.
- **The optional Goose CLI fallback runs the committed recipe**
  `scripts/goose-recipes/recipe.yaml` — a FLAT single agent with the full
  corpus-admin catalog and the confirm-token gate as the sole hard control
  (D1 verdict: goose subrecipe delegation does not load a subrecipe's
  extensions into subagents, so per-subagent grants are not enforceable via
  subrecipes; `available_tools` DOES filter when a recipe runs as its own
  main recipe — see dev/decisions.md, the chat-runtime closeout). Headless drive:
  `scripts/goose-chat run --recipe scripts/goose-recipes/recipe.yaml --params "request=<utterance>"`;
  interactive: `scripts/goose-chat --recipe scripts/goose-recipes/recipe.yaml`.
- **The app's native management chat** (dev/decisions.md, the chat-runtime
  closeout): goose's `Agent` runtime is EMBEDDED in-process as a source-level
  git dependency (pinned rev, Apache-2.0 — see dev/decisions.md for the
  deliberate-bump discipline and the ICS resolver pins). All GDK lives in `crates/corpus-app/src/chat/`
  (boundary: our own `ChatEvent`/`ChatCommand`/`Chat` trait; `embedded.rs`
  quarantines every goose type and drives the agent on a background thread,
  spawning `corpus-admin-mcp` as its tool extension — our own protocol, not
  a goose subprocess; never via `scripts/goose-chat`). The backend is gated
  behind the cargo feature `chat-embed` (default ON). The confirm ritual is
  IN-PROCESS and stronger than the old ACP arm: a mutating tool call is
  surfaced to the operator as an inline Approve/Reject and released via
  goose's `tool_confirmation_router` BEFORE dispatch — the model never sees a
  token (the corpus-mcp server-side token gate stays as backstop). Headless
  builds opt out with `--no-default-features`, which ALSO drops the goose dep
  tree (`goose` is an optional dep); with it OFF the backend is a no-op stub
  that reports the feature is required. The panel (`chat/panel.rs`, native
  egui) renders attributed message bubbles (you / corpus), collapsible
  thought cards (the model's reasoning, chronological in the log),
  collapsible tool-call cards, a live activity tail + stop button, and the
  confirm ritual as inline Approve/Reject (the operator releases every
  mutating tool call). The chat model picker lives in the panel footer by
  the input and is its OWN source — corpus-core
  `ollama_models()` (`ollama list`), because the chat talks to Ollama
  directly; the mission/agent picker keeps opencode's `model_list()`
  unchanged. New deps on corpus-app: `goose` (+ `tokio`/`tokio-util`/
  `futures`/`anyhow`/`rmcp`, and ICU resolver pins). The MANAGED `goose acp`
  subprocess arm it replaced is deleted (git history keeps it; the fallback
  story lives in dev/decisions.md).
  **TEAM SHAPE** (dev/chat-harness-plan.md): the panel runs a
  role-scoped session (`chat/team.rs` — a ROLE selector in the chat header,
  default **Operator**). `TeamRole` = `Operator` / `Orchestrator` /
  `AgentBuilder` / `ProjectManager` / `MissionManager` / `CorpusInspector`.
  Each non-`Operator` role registers `corpus-admin-mcp` with
  `available_tools` = its scoped domain, so a specialist is scoped BY
  CONSTRUCTION (goose's `is_tool_available` refuses out-of-domain tools). The
  destructive set (`corpus_wipe`/`project_delete`/`agent_delete`/
  `mission_delete`/`entry_delete`) is withheld from EVERY specialist and the orchestrator
  holds none (registers no admin extension); the Orchestrator delegates to
  specialists via OUR **`delegate` frontend tool** (`build_team_extension` in
  `chat/embedded.rs`): goose yields the call to the app, which spawns the
  specialist IN-PROCESS as a full goose Agent with its own scoped
  corpus-admin extension, streaming its tool calls as `role›tool` cards.
  (goose's summon platform extension was dropped 2026-08-14: a delegated
  subagent inherits only the PARENT session's extensions, so per-specialist
  scoping through summon is impossible — audit in dev/chat-harness-plan.md.)
  **Destructive ops are Operator-mode only** (the default role): the full
  catalog is present and every destructive call is gated by the inline
  Approve/Reject before dispatch. **Approval policy** (`chat/team.rs` —
  `needs_approval` over the pure-data whitelists `READ_ONLY_TOOLS` /
  `WRITE_TOOLS` / `DESTRUCTIVE_TOOLS`, partition-tested): read-only tools
  never ask (auto-released in-process — a smart agent reads freely); write
  tools gate while `CORPUS_CHAT_APPROVE_WRITES` is unset/non-`0` (the
  kill-switch NEVER covers the destructive set — that always gates);
  unknown tools fail closed. Delegated specialists run the same policy —
  their write calls surface as panel Approve/Reject cards routed back to
  the specialist agent by the pending-confirmation router
  (`deliver_confirmation`). Session transcripts flush to
  `<project scope>/var/chat/<session>.md` on each completed turn; per-turn
  harness diagnostics (lifecycle, tool calls, usage, errors) append to
  `<project scope>/var/chat/chat.log`.

The default write/read scope is project `default`
(`CORPUS_PROJECT` overrides it). Agents write into the project corpus
via the MCP tools. Launch knobs:
`CORPUS_NO_TMUX=1` forces the piped (no-attach) run backend (the app's
run view then shows the transcript tail instead of the embedded pane);
`CORPUS_TERMINAL` only shapes corpus-core's `attach_command()` helper
(external-terminal attach, retained for CLI use — the app never pops a
terminal).

Execution budget is a MISSION property (per-mission, in the mission
record frontmatter), not a per-agent or per-project one — the mission is
the launch unit. Agent configs carry model/description/prompt/permission
only.

A fresh agent should answer "how do I run the oracle suite?" with: `corpus
plugin probe <plugin>` (is the environment healthy), then via the MCP tools
call `oracle_list`, read the returned names and descriptions, and use
`oracle_run` for the relevant exact names. "Who may touch the sandbox?" →
the `tester` and `super` roles, via
the `corpus_sandbox_exec` MCP tool (see Trust domains below).

## Trust domains (hard rules)

| Zone | Who may do it | Egress | Never mounted into the sandbox |
|---|---|---|---|
| **Execution sandbox** | `tester` or `super`, via `corpus_sandbox_exec` | DENIED by default | benchmarks/, plugins/, oracle implementations, findings of the current mission |
| **Research zone** | `researcher` or `super` | open internet (webfetch/search) | read-denied on benchmarks/**; researcher executes nothing |
| **Project management** | `curator` or `super`, via scoped admin tools | Curator: no egress/sandbox; Super also holds both | manages only its proven project; cannot name another project; every act recorded |
| **Model inference** | host-side only, local by default | the model endpoint sits on the host | the sandbox has no model access |
| **Corpus store** | write via MCP tools (`finding_write`, `attack_save`, `technique_save`), gated per artifact | — | — |

Roles are `AgentRole::{Super, Curator, Tester, Researcher}` and are enforced twice:
server-side by corpus-mcp (which resolves the run's agent from
`CORPUS_OPENCODE_AGENT` + `CORPUS_PROJECT` and refuses everything outside
the ceiling), and in the rendered `.opencode/agent/*.md` by opencode
permissions DERIVED from the role at render time. The stored config can
only tighten, never widen. A tester's channel into the world is the MCP
tool catalog; a researcher can never execute. Super is the union of research,
testing, and project-scoped management. It can confirmation-gated wipe its own
corpus, but project creation/cloning/rebinding/deletion, cross-project copying,
and access to other projects remain operator-only. Every role, including Super,
is denied an unrestricted host shell because that would bypass project scope.

The rendered block is computed as a typed `Policy` value (agents.rs) and
serialized once, rather than assembled by mutating a JSON map. Every
failure that design shipped was a rule that silently did not land because
the stage meaning to write it had nowhere to write — a scalar `read:
"allow"` is not a map, so the red lines injected into `read` went nowhere;
`bash` was defaulted where it meant to be forced, so a stored `allow`
survived every render. A struct field cannot be absent, which is the point.
`tests/roles.rs` holds a byte fixture per role plus both halves of the
merge (a stored block may tighten; it may not widen).

## The super role

Super is the current-project union role: Researcher + Tester + scoped Curator,
plus confirmation-gated `corpus_wipe`. Its MCP catalog merges the sandbox and
management catalogs; management calls still pass through project injection and
the append-only audit log. It may create and host any project role. It is not a
host-global operator: project lifecycle, cross-project copying, other-project
access, unrestricted host shell, benchmark internals, and plugin internals stay
denied.

## The curator role

An agent that manages its own project rather than the target: it creates
and edits the project's other agents, sets their roles, writes mission
records, and curates the corpus. It holds ZERO sandbox tools and no open
internet — an agent that can rewrite every other agent's config is the last
one that should be reading attacker-controlled text.

- **Scoped by construction.** `tools::dispatch` routes the management tools
  through `scoped_management_dispatch`, which overwrites `args["project"]` from the
  proven scope before dispatching. The 17 sites in `admin.rs` that read
  `project` therefore all see the same unforgeable value, and the scoped
  catalog strips `project` from every advertised schema — a tool that
  quietly ignores an argument it asked for is worse than one that never
  asked.
- **Curator and Tester are different domains under Super.** `AgentRole` is
  deliberately not `Ord`; `cap_under` spells the relation out. A subagent under
  a Curator is Curator, narrower research roles cannot host Curator, and Super
  may host any project role because its server catalog contains both domains.
- **It may set non-super roles, including its own.** Creating, cloning,
  promoting, or editing a `super` agent is operator-owned. It may delete agents,
  missions, and corpus entries inside its injected project scope through the
  same dry-run/token gate as admin; every attempt and outcome is audit-recorded.
- **Every act is recorded.** `~/.corpus/var/audit/<project>.jsonl`, append
  only: intent before the call, outcome after, actor on every line. A
  failure to record REFUSES the call — an act this role cannot account for
  does not happen. The log lives outside the project subtree, so the agent
  it describes can neither edit it (`write` is deny-by-default with only
  its own corpus re-allowed) nor reach it with `entry_delete` (rooted at
  `corpus/`, category-gated). Read it with `corpus audit <project>`; no
  agent has a tool for it, because the subject of a log should not be its
  reader.
- **`runs/` is nobody's to change.** Technique cards cite those transcripts
  by name, `corpus_cost` counts them, and they are the provenance an
  operator audits. Denied to `entry_delete`/`entry_move` AND write-denied
  in every rendered permission block, since the corpus is the one place an
  agent may write with opencode's own file tools.
- **It may launch.** `mission_launch` starts a prepared project mission; that
  mutation is scoped and audit-recorded like its other management acts.

## Debugging a run: the refusal log

`~/.corpus/var/refusals/<project>.jsonl`, written by `corpus-mcp` at the
one door every tool call passes through (`tools::dispatch`). Every `Err`
that reaches a caller leaves a line — tool, args, actor, the role in force,
the message verbatim, the `run_log` basename that places it in the
transcript, and the **gate** that refused: `identity`, `role`, `scope`,
`probe`, `args`, `unknown`, `harness`. Read it with
`corpus refusals <project> [--gate G]`.

Read it BEFORE the transcript. The raw run capture is a PTY dump of a TUI —
megabytes of ANSI redraw in which the only account of a refusal is the
model's own prose about it. This is the same facts as values.

- **Empty is a finding.** No refusals for a run that misbehaved means the
  corpus server never turned it away, which narrows the hunt to opencode's
  permission block or to a tool description pointing somewhere the agent
  cannot reach. That is exactly the `/opt/src` namespace split: the mount
  path in `target_info` is the SANDBOX's, reachable only through
  `sandbox_exec`, while a host-launched agent has the same trees under
  `sources/<name>/<sha>` in its run cwd. An agent that believes the former
  gets denied by opencode, and corpus records nothing, because corpus did
  nothing.
- **`unknown` is not a permissions problem.** It is the only gate meaning
  the server had no opinion at all.
- **Best-effort, unlike the audit log.** `refusal::record` returns `()` and
  has no fallible public write path: `audit` REFUSES an act it cannot
  record, because a curator is trusted on the strength of the record; a
  diagnostic that could change an outcome would be an observer altering
  what it observes.
- **Out of reach of its subject, like the audit log.** It lives outside the
  project subtree and is read+write denied by path in every rendered
  permission block — readable, it is a map of every gate and the exact
  wording that trips it. `corpus refusals` is operator-only; no agent has a
  tool for it.
- Calls refused before a project could be resolved land in `_unscoped` —
  the most diagnostic records there are, so they are never dropped for want
  of a filename. The slug is sanitized into one path component, since a
  malformed `CORPUS_PROJECT` is itself a refusal worth logging.

## Plugin protocol

Any executable speaking newline-delimited JSON on stdio is a plugin (bash
qualifies). Protocol-v1 contracts, typed records, lifecycle, and session calls
live in `crates/corpus-core/src/plugin.rs` and `dev/architecture.md`. The
reference implementation is the separately released `corpus-plugin-cdk`
repository; Corpus retains fake protocol fixtures, not an editable production
adapter.

## Store conventions

- Categories: `store/projects/<project>/corpus/{hypotheses,techniques,findings,attacks,runs}/`
  on the **project-global** corpus (the ONLY corpus scope). The five
  categories are the machine-readable core (runs are harness-written,
  findings oracle-gated, techniques schema'd) — NOT a filing
  requirement: permissions cover `corpus/**`, so agents may invent any
  subpath; out-of-category files surface in the project view's `other`
  bucket.
- There are no seed agent documents. A new agent is created from a ROLE
  (`corpus agent new <p> <slug> --role super|curator|tester|researcher`), whose
  starting prompt is compiled into corpus-store
  (`crates/corpus-store/src/prompts/*.md`) and whose ceiling the renderer
  derives. Seeds were data pretending to be code: they shipped in the repo,
  drifted from the renderer, and gave every agent two sources of truth about
  what it could do. Fixtures for the render live in
  role fixtures remain in `crates/corpus-core/tests/fixtures/` — never in a directory opencode
  discovers — and re-bless with `CORPUS_BLESS=1 cargo test -p corpus-core
  --test roles`.
- Every render is **project-bound**: `store/projects/*` permission patterns
  are rewritten to the concrete project; the path rules exist even when the
  stored config omits them (silence never means allow); a read-allow gains
  the boundary rules — relative (`store/projects/*: deny`, own corpus and
  missions allowed) AND absolute (the data root denied, the one project's
  corpus re-allowed), because the run cwd's relative globs say nothing
  about `/Users/…/.corpus/...`; the trust red lines (`benchmarks/**`,
  `plugins/**`) are injected unconditionally; `task:` is confined to entries
  the project actually declares; and a "Corpus scope" section names the
  exact corpus.
- A rendered agent set must be **closed under delegation**: every name in a
  `task:` allowlist belongs to an agent the project declares. A dangling
  name is not inert — opencode resolves it from whatever config it finds
  above the run cwd, which is exactly how one project's primary came to
  delegate to another project's scout. `render_project_agents` refuses;
  `Store::check_project_delegation` is the same check for a pre-launch UI.
- Slugs are **kebab-case**, one card per technique (no `_` / title drift).
- `findings/` and `runs/` filenames carry an **epoch-seconds prefix**
  (`<epoch>-<slug>`); they read newest-first. Other categories are A–Z.
- Findings/techniques/hypotheses carry YAML frontmatter
  (`name`, `status`, `run_log`, `timestamp`); techniques cite the run log
  that produced them (`run_log` must name an existing file in the project
  corpus `runs/`).
- Every entry carries `sensitivity: open | internal | embargoed`, defaulted
  by the write tools (embargoed for findings, internal otherwise).
  `sensitivity:` stays in frontmatter as the classification future egress/
  redaction gates act on (roadmap data-security).
- **Contamination rule:** the store never references `benchmarks/**` or
  `plugins/**` internals; a run whose transcript reads them is
  contaminated and unscored.
- Attacks are **directories** (`attacks/<slug>/attack.md + run.sh`).
- Project corpus **wipe** (`corpus project wipe`) deletes the working
  subtree and bumps `corpus_generation` in the project spec; provenance
  strings (`agent@<config-hash>` + generation) keep old run logs
  attributable.

## Agent-building guidance

For corpus-app (egui) work, `dev/app-flow-plan.md` is authoritative: the
app is being redesigned against the operator's mocks in gated chunks
(theme/shell → sidebar → top bar → project → agent → mission views),
each chunk stopping for operator assessment. It rides the teamless data
model in `dev/data-model-plan.md`.

For anything touching the research loop (roles, tool contracts, sandbox
policy), `dev/architecture.md` "The research team" and "Trust domains"
are authoritative; `dev/roadmap-plan.md` is the honest state-of-the-world.

## Plan hygiene (house rule)

Any plan in `dev/` that becomes DONE gets collapsed into
`dev/decisions.md`
(one paragraph: what was decided, why, where the code lives) and deleted
from `dev/`. `dev/` keeps ACTIVE plans only; this keeps pointers in this
file and `dev/roadmap-plan.md` fresh.
