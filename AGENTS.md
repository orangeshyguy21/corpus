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
  corpus-deck/       egui desktop app (corpus-deck), the operator UI
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
store/               the corpus knowledge base. Core templates (versioned
                     with the app — the one committed part of store/) live
                     at store/templates/{permissions,prompts,agents}; the
                     data itself is scoped:
                     store/projects/<slug>/corpus/ (project-global, curated)
                     store/projects/<slug>/teams/<team>/corpus/ (team-scoped)
sources.toml         pinned target source manifest (repo → commit SHA)
sources/             git-ignored fetch of pinned trees (sources/<name>/<sha>/)
docs/                (gone — folded into dev/; see below)
dev/                 everything uncommitted & machine-local: architecture,
                     decisions, research, alpha-1, plus ACTIVE plans
                     (roadmap-plan, data-model-plan, deck-flow-plan)
                     and the demo poster. Git-ignored: may be absent on
                     a fresh clone.
.opencode/           opencode config + the role agents (operator, researcher,
                     generated from store/templates — do not hand-edit)
```

## Build / test / run commands

```bash
cargo build -p corpus-core -p corpus-mcp -p corpus-cli   # core + CLI
cargo build -p corpus-deck                                # the egui app
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
# Run a mission; transcript lands in the scoped corpus runs/ (default
# project/team); --research appends a researcher curation pass
corpus run <agent> [-m model] [--research] <mission...>
# Model registry
corpus models list
# Scoped store admin: projects, teams, templates, promotion
corpus project list|new|clone|delete
corpus team list|new|edit|clone|delete|wipe <project> ...
corpus template list|render     # render a template to .opencode/agent/<name>.md
corpus promote <project> <team> <category> <entry> [--confirm]
corpus store migrate [--dry-run]   # relocate a legacy flat store into
                                   # store/projects/<default>/corpus/ (dry-run
                                   # reports only; moves are checksum-verified)
```

The default write/read scope is project `default` / team `default`
(`CORPUS_PROJECT`/`CORPUS_TEAM` override it). Agents write into the team
scope via the MCP tools; entries reach the project-global corpus only via
`corpus_promote` (embargoed findings require `--confirm`).

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
  on the **project-global** corpus, and the same layout under each team's
  `store/projects/<project>/teams/<team>/corpus/`.
- Core templates live at `store/templates/{permissions,prompts,agents}/` and
  are the one committed part of store/ (`.gitignore` carves them out of the
  otherwise-private store). The role agents in `.opencode/agent/` are
  **generated** from them (`corpus template render`) — hand-editing both is
  drift.
- Slugs are **kebab-case**, one card per technique (no `_` / title drift).
- `findings/` and `runs/` filenames carry an **epoch-seconds prefix**
  (`<epoch>-<slug>`); they read newest-first. Other categories are A–Z.
- Findings/techniques/hypotheses carry YAML frontmatter
  (`name`, `status`, `run_log`, `timestamp`); techniques cite the run log
  that produced them (`run_log` must name an existing file in the team
  corpus `runs/`, project corpus `runs/` as fallback for migrated logs).
- Every entry carries `sensitivity: open | internal | embargoed`, defaulted
  by the write tools (embargoed for findings, internal otherwise). Entries
  leave a team scope only via `corpus_promote`; **embargoed entries require
  an explicit confirm flag**. Promotion records `promoted_from:
  <project>/<team>@<hash>/<generation>`.
- **Contamination rule:** the store never references `benchmarks/**` or
  `plugins/**` internals; a run whose transcript reads them is
  contaminated and unscored.
- Attacks are **directories** (`attacks/<slug>/attack.md + run.sh`).
- Team corpus **wipe** (`corpus team wipe`) deletes the working subtree and
  bumps `corpus_generation` in the team spec; provenance strings
  (`project/team@hash/generation`) keep old run logs attributable.

## Agent-building guidance

For corpus-deck work, `dev/deck-flow-plan.md` is authoritative: the deck
is being rebuilt ground-up in gated chunks (shell → projects → plugin →
teams → agents → launch → wizard), each chunk stopping for operator
assessment. The old M0 code and the absent egui plan carry no authority.

For anything touching the research loop (roles, tool contracts, sandbox
policy), `dev/architecture.md` "The research team" and "Trust domains"
are authoritative; `dev/roadmap-plan.md` is the honest state-of-the-world.

## Plan hygiene (house rule)

Any plan in `dev/` that becomes DONE gets collapsed into
`dev/decisions.md`
(one paragraph: what was decided, why, where the code lives) and deleted
from `dev/`. `dev/` keeps ACTIVE plans only; this keeps pointers in this
file and `dev/roadmap-plan.md` fresh.