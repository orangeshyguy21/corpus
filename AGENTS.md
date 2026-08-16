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
Cargo.toml           workspace root
crates/
  corpus-core/       core library: plugin protocol client, model registry,
                     discovery. This is where shared protocol types live.
  corpus-mcp/        MCP server exposing the corpus tools to agents
  corpus-cli/        headless CLI: run / plugin / models (+ ratatui TUI,
                     slated for removal once the deck covers M1+M2)
  corpus-app/        egui desktop app (corpus-app), the operator UI
plugins/
  cdk-regtest/       reference environment plugin (sandbox + oracles +
                     faucet + targets) for the CDK e-cash target; the
                     in-repo protocol fixture. Other plugins live in
                     their own repos (see roadmap #18).
benchmarks/
  models.yaml        model registry (identity + metadata; scores live in
                     benchmarks/results/<model>/*.yaml)
  forensic/          historical-bug forensic suite (CDK-BENCH-XXXX.yaml)
  results/           per-model benchmark score results
store/               the corpus knowledge base. Core seed agents (versioned
                     with the app — the one committed part of store/) live
                     at store/templates/agents/<slug>/{opencode.json, prompts/};
                     the management-chat recipe (goose, GDK) lives at
                     store/templates/chat/ (recipe.yaml + subrecipes/);
                     the data itself is scoped:
                     store/projects/<slug>/corpus/             (the corpus)
                     store/projects/<slug>/agents/<slug>/      (agent configs)
                     store/projects/<slug>/missions/<slug>.md  (mission records)
sources.toml         pinned target source manifest (repo → tag + sha; the
                     DEFAULT pin per repo — the PLUGIN defines the revs
                     available, the PROJECT owns the pick (persisted on
                     project.yaml `pins`): the top-bar dropdown discovers
                     tags via git ls-remote (cached under
                     sources/.rev-cache/), launch resolves rev → sha +
                     fetches the tree, and the sandbox is recreated when
                     its mounts don't match the mission's pins, delivered
                     via CORPUS_SOURCE_PINS)
sources/             git-ignored fetch of pinned trees (sources/<name>/<sha>/)
docs/                (gone — folded into dev/; see below)
dev/                 everything uncommitted & machine-local: architecture,
                     decisions, research, alpha-1, plus ACTIVE plans
                     (roadmap-plan, data-model-plan, app-flow-plan,
                     app-parity-spec, mission-view-plan)
                     and the demo poster. Git-ignored: may be absent on
                     a fresh clone.
.opencode/           opencode config + the role agents (operator, researcher,
                     generated from store/templates/agents — do not hand-edit)
```

## Build / test / run commands

```bash
cargo build -p corpus-core -p corpus-mcp -p corpus-cli   # core + CLI
cargo build -p corpus-app                                # the egui app
cargo test -p corpus-core -p corpus-mcp                   # unit + scoped-store tests
cargo test -p corpus-app --bin corpus-app                 # app + chat/team injection probes
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

The `cdk-regtest` plugin is a bash script plus a Docker-based arena; it is
not built by cargo. One-shot setup (fetch pinned sources, build agent
image + tools, run the doctor self-verification probes):

```bash
plugins/cdk-regtest/setup.sh
```

Environment is checked via `corpus plugin probe cdk-regtest`. The CLI:

```bash
# Discover plugins / probe the environment / make raw protocol calls
corpus plugin list
corpus plugin probe <name>
corpus plugin call <name> <method> [params-json]
# Run a mission; the project's agents are materialized to the PROJECT's
# OWN .opencode/agent/ inside its run directory
# (store/projects/<p>/var/run/ — provisioned per launch with symlinks to
# the repo's store/, sources/, and .opencode/opencode.json so relative
# paths and the MCP config resolve). opencode runs with that dir as cwd:
# each project owns its agent set AND its opencode session pool — one
# project never overwrites another's materialized agents. The app
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
corpus agent list|new|clone|delete <project> ...
corpus mission list|new|delete <project> ...
corpus store migrate [--dry-run] [--project <slug>] [--confirm]
                                  # relocate a legacy flat store into
                                  # store/projects/<default>/corpus/ (dry-run
                                  # reports only; moves are checksum-verified)
                                  # --confirm also removes legacy template dirs
# corpus-admin MCP profile: the SAME corpus-mcp binary behind `--admin`
corpus-mcp --admin                 # stdio MCP server; 21 admin tools only
```

## corpus-admin MCP profile (dev/decisions.md — chat runtime closeout)

`corpus-mcp --admin` is a second trust profile of the same binary —
host-side operator tooling, thin over corpus-core, for natural-language
store administration via the GDK/goose chat. It sits OUTSIDE the research
trust domains (no sandbox, no oracles, no targets) and never runs missions
— it prepares them. The sandbox-facing profile (operator/researcher
agents) never enables it; the chat session config always does
(`corpus-mcp --admin`). The tool group lives in
`crates/corpus-mcp/src/admin.rs`; `main.rs` gates `tools/list` + `tools/call`
on the flag and the admin profile advertises NO sandbox/finding tools.

Admin tools (all thin over corpus-core): `project_list/new/clone/delete/
rebind`, `agent_list/get/new/save/clone/delete` (`agent_new` builds the
opencode.json from structured fields — prefer it for creation; `agent_save`
only edits existing agents and runs the core validator, refusing invalid
documents with the validator's message), `mission_list/
get/new/delete/set_budget/set_pins`, `corpus_wipe`, `corpus_stats/list/read`,
`model_list` (discover opencode model ids for agent configs).

- **Confirm-token gate (server-side, all four destructive ops):**
  `project_delete`, `agent_delete`, `mission_delete`, `corpus_wipe` first
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
  registry validation, and admin tools absent without `--admin`.

### GDK/goose chat wiring (chunk 1, local Ollama)

Goose config (`~/.config/goose/config.yaml`) declares the admin extension
and disables the shell:

```yaml
extensions:
  corpus-admin:
    type: stdio
    name: corpus-admin
    enabled: true
    cmd: /Users/admin/Sites/corpus/target/debug/corpus-mcp
    args: ["--admin"]
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
  `store/projects/<p>/var/chat/data/sessions/`. ALWAYS launch the chat
  through the wrapper `scripts/goose-chat [goose args...]` (scope =
  `CORPUS_PROJECT`, default `default`) — it provisions the scope's own
  config.yaml (provider + corpus-admin + developer-off, `GOOSE_INPUT_LIMIT`
  at root) and execs goose with `GOOSE_PATH_ROOT` set. Example:
  `scripts/goose-chat run -n ops -t "<prompt>"`. A raw `goose run` outside
  the wrapper leaks sessions to the default dir.
- **The management chat runs the committed recipe**
  `store/templates/chat/recipe.yaml` — a FLAT single agent with the full
  corpus-admin catalog and the confirm-token gate as the sole hard control
  (D1 verdict: goose subrecipe delegation does not load a subrecipe's
  extensions into subagents, so per-subagent grants are not enforceable via
  subrecipes; `available_tools` DOES filter when a recipe runs as its own
  main recipe — see dev/decisions.md, the chat-runtime closeout). Headless drive:
  `scripts/goose-chat run --recipe store/templates/chat/recipe.yaml --params "request=<utterance>"`;
  interactive: `scripts/goose-chat --recipe store/templates/chat/recipe.yaml`.
- **The app's native management chat** (dev/decisions.md, the chat-runtime
  closeout): goose's `Agent` runtime is EMBEDDED in-process as a source-level
  git dependency (pinned rev, Apache-2.0 — see dev/decisions.md for the
  deliberate-bump discipline and the ICS resolver pins). All GDK lives in `crates/corpus-app/src/chat/`
  (boundary: our own `ChatEvent`/`ChatCommand`/`Chat` trait; `embedded.rs`
  quarantines every goose type and drives the agent on a background thread,
  spawning `corpus-mcp --admin` as its tool extension — our own protocol, not
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
  Each non-`Operator` role registers `corpus-mcp --admin` with
  `available_tools` = its scoped domain, so a specialist is scoped BY
  CONSTRUCTION (goose's `is_tool_available` refuses out-of-domain tools). The
  destructive set (`corpus_wipe`/`project_delete`/`agent_delete`/
  `mission_delete`) is withheld from EVERY specialist and the orchestrator
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
plugin probe cdk-regtest` (is the environment healthy) then via the MCP
tools `oracle_run` per oracle reported by `corpus plugin call cdk-regtest
oracles`. "Who may touch the sandbox?" → only the **operator** agent, via
the `corpus_sandbox_exec` MCP tool (see Trust domains below).

## Trust domains (hard rules)

| Zone | Who may do it | Egress | Never mounted into the sandbox |
|---|---|---|---|
| **Execution sandbox** | the `operator` agent, via `corpus_sandbox_exec` | DENIED by default | benchmarks/, plugins/, oracle implementations, store/findings of the current mission |
| **Research zone** | the `researcher` agent (reads only) | open internet (webfetch/search) | executes nothing; read-denied on benchmarks/** |
| **Model inference** | host-side only, local by default | the model endpoint sits on the host | the sandbox has no model access |
| **Corpus store** | write via MCP tools (`finding_write`, `attack_save`, `technique_save`), gated per artifact | — | — |

Roles are enforced in `.opencode/agent/` by opencode permissions, not
vibes. The operator has all host reads denied; its only channel into the
world is the MCP tool catalog. The researcher can never execute.

## Plugin protocol

Any executable speaking newline-delimited JSON on stdio is a plugin
(bash qualifies). The client lives in `crates/corpus-core/src/plugin.rs`;
the full method table (`probe`, `up/down`, `targets`, `tools`, `sources`,
`sandbox_exec`, `oracles`, `call_oracle`, `faucet`) is documented in
`crates/corpus-core/src/plugin.rs` and `dev/architecture.md`.
The reference plugin is `plugins/cdk-regtest`.

## Store conventions

- Categories: `store/projects/<project>/corpus/{hypotheses,techniques,findings,attacks,runs}/`
  on the **project-global** corpus (the ONLY corpus scope). The five
  categories are the machine-readable core (runs are harness-written,
  findings oracle-gated, techniques schema'd) — NOT a filing
  requirement: permissions cover `corpus/**`, so agents may invent any
  subpath; out-of-category files surface in the project view's `other`
  bucket.
- Core seed agents live at `store/templates/agents/<slug>/` (opencode.json +
  prompts/) and are the one committed part of store/ (`.gitignore` carves them
  out of the otherwise-private store). The role agents in `.opencode/agent/`
  are **generated** from them — regenerate with
  `cargo run -p corpus-core --example render_seeds` after editing a seed;
  hand-editing both is drift (the `templates` test enforces byte equality).
  Every render is **project-bound**: `store/projects/*` permission patterns
  are rewritten to the concrete project, a wildcard read-allow gains the
  boundary rules (`store/projects/*: deny`, own-project allow), the trust
  red lines (`benchmarks/**`, `plugins/**` read denies) are injected
  unconditionally (they cannot be edited out of an agent JSON), and a
  "Corpus scope" section is appended naming the exact corpus — a launched
  agent cannot read or write another project's corpus.
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