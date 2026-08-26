# AGENTS.md — Corpus contributor guide

Corpus is a local-first vulnerability research platform. AI researchers work
against operator-authorized projects at pinned commits, through independently
released environment plugins, and write attributable evidence to a local
corpus store.

This file is for code contributors. Keep audience-specific procedure in its
authoritative owner:

- [`docs/operator-guide.md`](docs/operator-guide.md) — routine operation;
- [`docs/troubleshooting.md`](docs/troubleshooting.md) — symptom-first recovery;
- [`docs/testing.md`](docs/testing.md) — hermetic, platform, and serial MLX tests;
- [`PLUGINS.md`](PLUGINS.md) — plugin installation, lifecycle, and authoring;
- [`docs/architecture.md`](docs/architecture.md) — shipped architecture;
- [`docs/threat-model.md`](docs/threat-model.md) — security invariants.

## Workspace ownership

```text
crates/
  corpus-store/       filesystem data model and durable records
  corpus-observe/     read-only host projections
  corpus-core/        plugin, source, launch, process, compatibility facade
  corpus-admin/       typed host/scoped administration policy and handlers
  corpus-admin-mcp/   host-side administration MCP executable
  corpus-mcp/         project-scoped research and management MCP server
  corpus-cli/         typed headless operator CLI
  corpus-app/         egui shell, state, jobs, and embedded chat
  corpus-model-test/  global local-model lease and Qwen MLX preflight
  corpus-integration/ hermetic and live end-to-end scenarios
benchmarks/           shipped model registry and forensic fixtures
scripts/              policy gates and optional Goose wrapper/recipes
docs/                 tracked architecture, operations, tests, and decisions
dev/                  ignored machine-local active design work; may be absent
.opencode/            repository development skills only
```

The exact normal dependency graph is documented in `docs/architecture.md` and
enforced by `scripts/check-dependency-policy`. In particular:

- `corpus-admin` depends only on `corpus-store` and `corpus-observe`;
- the host admin MCP cannot gain core, plugin, launch, app, or research-MCP
  authority;
- `corpus-store` is the durable leaf and `corpus-observe` is read-only;
- executables compose capabilities instead of moving them into a shared
  all-authority facade.

Do not add a workspace edge without updating and deliberately reviewing both
the architecture and executable dependency policy.

## Data and namespace invariants

`CORPUS_HOME` defaults to `~/.corpus` and contains operator-owned mutable data.
`CORPUS_RESOURCES` identifies shipped read-only assets. `corpus-store` owns root
resolution; never infer either root through `store.root().parent()` or the
current working directory.

Each run workspace exposes exactly one project plus its pinned sources. It is
outside the Corpus repository so upward OpenCode configuration discovery
cannot find this repository's `.opencode/`. Project-level rendered agent files
contain only stable project facts. Per-run agent, transcript, source-pin,
mission, and origin identity travels through the launch environment.

Filesystem absence is the primary boundary: another project, benchmarks,
plugin internals, and oracle implementations must not be mounted into a run or
sandbox merely because a deny glob also exists. All model-, tool-, plugin-, and
stored path input is untrusted until confined beneath its capability root.

## Build and verification

Common contributor commands are:

```sh
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo build --locked -p corpus-app --no-default-features
./scripts/check-dependency-policy
./scripts/check-supply-chain
cargo fmt --all -- --check
git diff --check
```

The default suite stays hermetic. Never start a model, Docker, OpenCode, or a
usable tmux server from a non-ignored test. The suite taxonomy and approved
live-model commands are in `docs/testing.md`.

All real model scenarios use only `qwen3.8:27b-mlx`. They acquire the shared
cross-process model lease and run serially; `--test-threads=1` alone is not
sufficient. Do not run ignored live tests while another model test may be
active.

Goose remains an intentional, source-pinned optional dependency behind the
application's default `chat-embed` feature. Do not remove it or change its
revision as dependency cleanup. Use its pinned Rust toolchain for the embedded
configuration; use `--no-default-features` for hermetic/headless app work that
does not exercise chat.

## Security change rules

`docs/threat-model.md` is authoritative. Changes must preserve its stable
`SEC-*` invariants and update their enforcement/test map when ownership moves.

| Zone | Authority | Hard boundary |
|---|---|---|
| Host operator | Local project and installation administration | Destructive actions require fresh confirmation |
| Project management | Curator or Super in one proven project | Cannot name another project or become host operator |
| Research | Researcher or Super | No execution and no benchmark access |
| Sandbox execution | Tester or Super through plugin tools | No model, benchmark, plugin, oracle, or current finding internals |
| Model inference | Host side | Never exposed inside the sandbox |
| Plugin process | Deliberately installed host-trusted code | Replies remain untrusted protocol data; execution is bounded |

Roles are server-derived from `CORPUS_PROJECT` and
`CORPUS_OPENCODE_AGENT`, then enforced again by a rendered typed policy. Stored
permissions may tighten the role ceiling but never widen it. Super is the union
of project-scoped capabilities, not a host-global operator. Every scoped
management mutation is audited; a mutation that cannot be audited refuses.

Keep these ownership rules explicit:

- server catalogs define authority; client schemas are not authorization;
- scoped management overwrites project arguments with proven scope before a
  handler reads them;
- stale UI and job results carry generation or request identity and refuse;
- durable teardown identity survives failure and restart;
- destructive confirmation is exact-target, expiring, and single-use;
- plugin and child-process calls have deadlines, output caps, and kill/reap;
- mission completion returns only to the exact persisted origin session.

## Store and agent contracts

The project-global corpus has conventional `hypotheses`, `techniques`,
`findings`, `probes`, and `runs` categories. Agents may create other corpus
subpaths when policy allows it. `runs/` is harness-owned provenance and cannot
be edited by agents.

- Project and entry slugs are kebab-case and one validated path component.
- Finding and run names carry an epoch-seconds prefix and sort newest-first.
- Findings, techniques, and hypotheses use validated YAML frontmatter.
- Techniques cite an existing project run log.
- Entries carry `open`, `internal`, or `embargoed` sensitivity.
- Probes are directories containing `probe.md` and `run.sh`.
- Corpus wipe increments `corpus_generation`; old provenance stays attributable.
- Persisted YAML compatibility follows
  [`docs/yaml-compatibility.md`](docs/yaml-compatibility.md).

Agents are created from `AgentRole::{Super, Curator, Tester, Researcher}`; there
are no shipped seed documents. Role prompts live in
`crates/corpus-store/src/prompts/`. Rendering always injects project scope,
trust red lines, and a delegation allowlist closed over agents declared by the
project. A dangling delegate is a launch failure, not an inert entry.

Role fixtures live under `crates/corpus-core/tests/fixtures/`, outside any
directory OpenCode discovers. Re-bless only after reviewing the policy change:

```sh
CORPUS_BLESS=1 cargo test -p corpus-core --test roles
```

## Change discipline

- Separate mechanical movement from behavioral changes.
- Add characterization tests before changing unclear behavior.
- Run the verification tier required by `docs/testing.md` after each slice.
- Never relax a security assertion merely to complete an extraction.
- Do not treat line-count reduction as proof of better boundaries.
- Prefer an existing dependency when it safely replaces custom machinery; add
  or retain one only with an explicit owner and supply-chain rationale.
- Do not create a crate unless its boundary is enforceable and documented.
- Preserve first-attempt evidence for flaky, platform, and model failures.
- Do not edit production plugin implementations here; fixtures define
  compatibility and plugins release independently.
- Keep Goose behind the Corpus-owned chat boundary until its intended crate
  migration is deliberately available.

For UI work, follow an active machine-local design plan when present, but never
make shipped behavior depend on `dev/`. For research-loop, role, MCP, sandbox,
source, launch, or completion changes, reconcile the tracked architecture and
security contract before implementation.

Completed temporary plans in `dev/` are summarized into tracked decisions and
removed. `dev/` contains active scratch work only and is absent from a fresh
checkout.
