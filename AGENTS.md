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
  cdk-regtest/       first-party environment plugin (sandbox + oracles +
                     faucet + targets) for the CDK e-cash target
  nutshell-fake/     minimal fake plugin example (plugin protocol reference)
benchmarks/
  models.yaml        model registry (identity + metadata; scores live in
                     benchmarks/results/<model>/*.yaml)
  forensic/          historical-bug forensic suite (CDK-BENCH-XXXX.yaml)
  results/           per-model benchmark score results
store/               the corpus knowledge base (hypotheses, techniques,
                     findings, attacks, runs)
sources.toml         pinned target source manifest (repo → commit SHA)
sources/             git-ignored fetch of pinned trees (sources/<name>/<sha>/)
docs/                architecture.md, decisions.md, research.md, alpha-1.md
dev/                 ACTIVE plans only (roadmap-plan, corpus-deck-egui-plan)
.opencode/           opencode config + the role agents (operator, researcher)
```

## Build / test / run commands

```bash
cargo build -p corpus-core -p corpus-mcp -p corpus-cli   # core + CLI
cargo build -p corpus-deck                                # the egui app
cargo test -p corpus-core -p corpus-deck                  # unit tests
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
# Run a mission; transcript lands in store/runs/; --research appends a
# researcher curation pass
corpus run <agent> [-m model] [--research] <mission...>
# Model registry
corpus models list
```

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
`crates/corpus-core/src/plugin.rs` and `docs/architecture.md`.
The reference plugin is `plugins/cdk-regtest`.

## Store conventions

- Categories: `store/{hypotheses,techniques,findings,attacks,runs}/`.
- Slugs are **kebab-case**, one card per technique (no `_` / title drift).
- `findings/` and `runs/` filenames carry an **epoch-seconds prefix**
  (`<epoch>-<slug>`); they read newest-first. Other categories are A–Z.
- Findings/techniques/hypotheses carry YAML frontmatter
  (`name`, `status`, `run_log`, `timestamp`); techniques cite the run log
  that produced them (`run_log` must name an existing file in store/runs/).
- **Contamination rule:** the store never references `benchmarks/**` or
  `plugins/**` internals; a run whose transcript reads them is
  contaminated and unscored.
- Attacks are **directories** (`attacks/<slug>/attack.md + run.sh`).

## Agent-building guidance

For corpus-deck work, read `dev/corpus-deck-egui-plan.md` §"UI
implementation guidance" BEFORE writing any view — it is the hard-won
list of M0 mistakes and the house style for egui panels, repaint
cadence, and store semantics.

For anything touching the research loop (roles, tool contracts, sandbox
policy), `docs/architecture.md` "The research team" and "Trust domains"
are authoritative; `dev/roadmap-plan.md` is the honest state-of-the-world.

## Plan hygiene (house rule)

Any plan in `dev/` that becomes DONE gets collapsed into `docs/decisions.md`
(one paragraph: what was decided, why, where the code lives) and deleted
from `dev/`. `dev/` keeps ACTIVE plans only; this keeps pointers in this
file and `dev/roadmap-plan.md` fresh.