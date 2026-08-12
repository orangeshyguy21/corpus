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
                     the data itself is scoped:
                     store/projects/<slug>/corpus/             (the corpus)
                     store/projects/<slug>/agents/<slug>/      (agent configs)
                     store/projects/<slug>/missions/<slug>.md  (mission records)
sources.toml         pinned target source manifest (repo → commit SHA)
sources/             git-ignored fetch of pinned trees (sources/<name>/<sha>/)
docs/                (gone — folded into dev/; see below)
dev/                 everything uncommitted & machine-local: architecture,
                     decisions, research, alpha-1, plus ACTIVE plans
                     (roadmap-plan, data-model-plan, app-flow-plan)
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
cargo build --workspace                                   # everything
```

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
# Run a mission; the agent is materialized to
# .opencode/agent/ first (bare names), then opencode runs on the
# CORPUS_PROJECT scope. The app launches the FULL opencode TUI in a
# DETACHED tmux session (corpus-<agent>-<ts>) and shows it in the
# EMBEDDED terminal pane (egui_term; the pane runs `tmux attach`
# in-process — no external-terminal popup): click the pane to steer,
# and the run survives attach/detach/app-close; a relaunched app lists
# live corpus-* sessions for in-pane re-attach. `corpus run` is the
# HEADLESS `opencode run` (automation): transcript .log in the project
# corpus runs/. Transcript of record for TUI runs: Dismiss/abort
# exports the session to <epoch>-<agent>.json in the project corpus
# runs/; the live tail is tmux pipe-pane raw capture
# (ANSI-stripped). The model is ALWAYS explicit (primary agent entry
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
```

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
  on the **project-global** corpus (the ONLY corpus scope).
- Core seed agents live at `store/templates/agents/<slug>/` (opencode.json +
  prompts/) and are the one committed part of store/ (`.gitignore` carves them
  out of the otherwise-private store). The role agents in `.opencode/agent/`
  are **generated** from them — hand-editing both is drift.
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