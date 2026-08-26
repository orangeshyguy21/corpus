# Corpus architecture

Status: shipped architecture after the senior-developer refactor  
Last verified: 2026-08-26

This document is the source of truth for code ownership, workspace dependency
direction, runtime composition, and durable-data boundaries. Security controls
are specified in [`threat-model.md`](threat-model.md), test environments in
[`testing.md`](testing.md), and dependency custody in
[`supply-chain-policy.md`](supply-chain-policy.md).

## System shape

Corpus is a local desktop and command-line application around a filesystem
store. It launches OpenCode missions with explicit project, agent, mission,
model, source-pin, environment-session, and run identities. Research tools are
served over a project-scoped MCP process; operator administration uses a
separate host-global MCP process. Plugins own project-specific sandbox and
oracle behavior behind a versioned process protocol.

The main runtime flow is:

```text
operator
  |-- corpus-app ----------------------> AppState
  |      |                                 |-- store/read projections
  |      |                                 |-- launch/session coordination
  |      `-- embedded Goose adapter        `-- curator completion delivery
  |              |
  |              `-- corpus-admin-mcp --> corpus-admin --> store/observe
  |
  `-- corpus CLI ----------------------> corpus-core
                                             |
                                             |-- project-scoped run workspace
                                             |-- OpenCode (tmux TUI or piped)
                                             `-- corpus-mcp --> plugin + store
```

The model is never an authority boundary. Schemas help it form requests, but
Rust handlers validate scope, role, paths, confirmation, origin, lifecycle,
and persistence preconditions below the model-facing layer.

## Workspace dependency graph

Arrows point from a consumer to a normal workspace dependency:

```text
corpus-app ----------> corpus-core ------> corpus-observe --> corpus-store
     |                      `------------> corpus-store
     `----------------> corpus-observe

corpus-cli ----------> corpus-core

corpus-mcp ----------> corpus-core
     `---------------> corpus-admin -----> corpus-observe --> corpus-store
                              `----------> corpus-store

corpus-admin-mcp ----> corpus-admin
```

The exact normal workspace edges are enforced by
`scripts/check-dependency-policy`. `corpus-integration` and
`corpus-model-test` are verification support packages rather than shipped
runtime layers; the gate checks their edges too.

| Crate | Responsibility | Allowed normal workspace dependencies |
|---|---|---|
| `corpus-store` | Durable records, checked filesystem transitions, paths, YAML boundary, audit/refusal state | none |
| `corpus-observe` | Read projections for models, plugins, sessions, pins, and run activity | `corpus-store` |
| `corpus-core` | Compatibility facade plus plugin protocol, installation, source revisions, launch, process, tmux, and transcript ownership | `corpus-store`, `corpus-observe` |
| `corpus-admin` | Typed operator/scoped administration catalog, policy, confirmation, and dispatch | `corpus-store`, `corpus-observe` |
| `corpus-admin-mcp` | Host-global administration MCP executable | `corpus-admin` |
| `corpus-mcp` | Launcher-scoped research MCP, sandbox/oracle/finding tools, and scoped admin adapter | `corpus-core`, `corpus-admin` |
| `corpus-cli` | Typed headless command surface | `corpus-core` |
| `corpus-app` | Desktop shell, render state, jobs, chat adapter, session coordination, diagnostics, and views | `corpus-core`, `corpus-observe` |

Two dependency decisions are security-significant:

1. `corpus-admin` does not depend on `corpus-core`, plugin execution, launch,
   the desktop app, research MCP, or Goose. The host admin server can prepare
   missions but cannot run them.
2. `corpus-store` is the leaf. Persistence rules do not import UI, process,
   protocol, or model behavior.

The app's default `chat-embed` feature adds the external pinned Goose runtime.
Goose types are quarantined in `corpus-app/src/chat/embedded.rs`; the rest of
the app consumes Corpus-owned chat commands and events. Headless builds disable
that feature. Goose stays until its planned crate migration.

## Executable surfaces

| Executable | Audience and scope | Composition |
|---|---|---|
| `corpus-app` | Host operator; all selected local projects | Egui shell, `AppState`, embedded management chat, background jobs, diagnostics |
| `corpus` | Host operator and automation | Typed CLI over `corpus-core` |
| `corpus-admin-mcp` | Host-global management chat | Full operator admin catalog over store/observe; no plugin or launch runtime |
| `corpus-mcp` | One launched research run | Role-filtered research tools and scoped admin subset using launcher-proven identity |

Both MCP executables use newline-delimited JSON-RPC over stdio and expose only
`initialize`, `ping`, `tools/list`, and `tools/call` behavior required by their
profiles. They are separate artifacts because project-scoped research and
host-global administration must not share ambient authority.

## Ownership inside the major crates

### Application

Views render projections and request actions; they do not access the store or
filesystem directly. `AppState` is the coordination boundary:

| Owner | Responsibility |
|---|---|
| `state/models.rs` | Render-safe projections without filesystem, process, session, or job behavior |
| `state/resources.rs` | Project, agent, mission, selection, source, and environment coordination |
| `state/corpus.rs` | Finding, cost, and corpus-summary projections and invalidation |
| `state/plugin.rs` | Plugin discovery, operations, and durable environment leases |
| `state/run.rs` and `state/run/coordinator.rs` | Owned run state, asynchronous launch, maintenance, and teardown |
| `state/session.rs` | External session discovery, activity, and repaint policy |
| `state/dispatch.rs` | Durable curator requests, child completion, delivery, acknowledgement, and recovery |
| `state/background.rs` | Job runtime, stale-scope rejection, invalidation, and result routing |
| `jobs.rs` | Bounded background execution, cancellation, deadlines, and terminal delivery |
| `session_service.rs` | OpenCode session HTTP/CLI adapter and exact turn/message identity |
| `diagnostics.rs` / `observability.rs` | Bounded local JSONL sink and stable lifecycle/delivery events |

### Store

The store separates records and policy by resource: projects, agents,
missions, findings, corpus entries, accounting, preferences, environment
sessions, run records, audit, and refusals. `filesystem.rs` owns atomic and
exclusive write mechanics. `run_workspace.rs` owns the project-only execution
namespace. `yaml.rs` is the sole Corpus YAML backend boundary.

Agent storage is further split into model, validation, roles, permissions,
rendering, mutations, and repository operations. Mission and run-record tests
live beside their production owners rather than in one store-wide test file.

### Launch and plugins

`corpus-core::launch` has one immutable plan and distinct adapters:

| Owner | Responsibility |
|---|---|
| `plan.rs` | Complete launch identity and backend-independent intent |
| `policy.rs` | Agent handles, explicit model resolution, and private control identity |
| `command.rs` | Plan-derived child environment and OpenCode command construction |
| `executables.rs` | Stable executable discovery and capability checks |
| `process.rs` | Bounded subprocess output, process-group cleanup, and durable piped logs |
| `tmux.rs` | Tmux argv and detached-session setup |
| `transcript.rs` | Artifact naming, raw capture, OpenCode session discovery, and export |
| `session.rs` | Backend state, observation, and checked teardown |
| `start.rs` | TUI/headless construction and compatibility routing |

Plugin discovery and read projections live in `corpus-observe`; protocol
negotiation, spawning, lifecycle, sandbox, oracle, source preparation, and
installation verification live in `corpus-core`. The app requests these
operations through state jobs and never spawns a plugin while painting.

### Administration and research tools

`corpus-admin` owns the typed tool registry, generated schemas, policy
metadata, confirmation tokens, and handlers. Project/agent/mission/corpus/model
domains have focused modules, and each mutation is classified for audit,
confirmation, refresh, and destructive authority.

`corpus-admin-mcp` advertises the host-global catalog. `corpus-mcp` uses the
same definitions through a scoped adapter, injects the launcher-proven project
and mission origin, and exposes only the role's allowed catalog. Research-only
sandbox, target, oracle, faucet, and finding behavior remains in
`corpus-mcp::tools`.

## Durable data and execution namespaces

Corpus has a writable data root and a separate read-only resource root. They
must never be inferred from one another.

```text
~/.corpus/                              CORPUS_HOME (default)
  store/projects/<project>/
    project.yaml
    agents/<agent>/
    missions/<mission>.md
    corpus/{hypotheses,techniques,findings,attacks,retro,runs}/
    usage/<session>.json
  cache/sources/<source>/<sha>/
  plugins/<plugin>/<version>/
  var/run/<project>/
    .opencode/agent/                  project-stable render staging
    views/sources-<sha256>/           launch cwd for one exact pin set
      store/projects/<project>        sole project link
      sources/<source>/<sha>          exact read-only pin links
      .opencode/                      snapshotted agents + local MCP config
  var/chat/<project>/
  var/plugins/
  var/audit/<project>.jsonl
  var/refusals/<project>.jsonl
  var/diagnostics/
  app.yaml
```

The resource root contains shipped assets such as `benchmarks/models.yaml` and
optional OpenCode skills. It is read-only and replaceable during an upgrade.

Before launch, the store renders project-stable agents beneath
`var/run/<project>`, then provisions a pin-keyed launch view beneath
`views/sources-<sha256>`. The launch cwd contains exactly one linked project
and only the exact `<source>/<sha>` trees from its immutable launch plan; the
shared source cache remains host-only. Different pin sets therefore cannot
race over one `sources` link. Per-run identity stays in the child/tmux
environment. An unexpected project or source entry, symlinked boundary or
source-tree component, unresolved source revision, or absent explicit model
aborts launch.

## Mission and curator lifecycle

1. An operator or curator persists a mission with an agent, optional budget,
   source pins, and immutable request origin.
2. The app or CLI resolves the explicit model, validates plugin/environment
   state, provisions the project-only workspace, and constructs one launch
   plan.
3. OpenCode runs in a detached tmux TUI when available or a supervised piped
   process otherwise. Raw output is durable from first output; structured
   export is best-effort during teardown.
4. The project-scoped MCP reconstructs authority from launch environment—not
   from model arguments or the currently selected UI project.
5. Child terminal state is persisted with exact run identity. The app groups
   completions by exact curator origin and sends a deterministic completion
   message to that curator session.
6. Acknowledgement advances durable delivery state only when message and run
   identities still match. Failure or restart retains bounded retry state.

The real Qwen3.8 MLX integration campaign runs these steps serially because the
prepared local runner can host only one model inference at a time. Production
identity and delivery rules do not depend on that test-runner limitation.

## Cross-cutting controls

- Filesystem writes use validated slugs/relative paths, no-follow traversal,
  collision-safe publication, and atomic or exclusive replacement.
- Child processes have explicit executable resolution, bounded output and
  deadlines, owned process groups, and checked cleanup identities.
- Audit and refusal logs live outside project-writable corpus trees.
- Destructive operator actions use single-use confirmation tokens tied to the
  exact preview and target state.
- Lifecycle and curator-delivery events carry stable project, mission, run,
  operation, outcome, retryability, and error fields into bounded local JSONL.
- Dependency sources, licenses, advisories, and duplicate versions are checked
  by `deny.toml`; workspace architecture edges are checked separately by the
  dependency-policy script.

## Verification and documentation map

| Concern | Authoritative document |
|---|---|
| Architecture and dependency direction | this document |
| Durable architectural rationale | [`decisions.md`](decisions.md) |
| Security boundaries and abuse cases | [`threat-model.md`](threat-model.md) |
| Routine installation and operation | [`operator-guide.md`](operator-guide.md) |
| Failure diagnosis and recovery | [`troubleshooting.md`](troubleshooting.md) |
| Hermetic, platform, and serial MLX suites | [`testing.md`](testing.md) |
| Dependency/advisory exceptions | [`supply-chain-policy.md`](supply-chain-policy.md) |
| Plugin installation and protocol | [`../PLUGINS.md`](../PLUGINS.md) |
| YAML persisted-data compatibility | [`yaml-compatibility.md`](yaml-compatibility.md) |
| Typed tool registry migration record | [`tool-registry-inventory.md`](tool-registry-inventory.md) |
| Refactor history and measured checkpoints | [`refactor-baseline.md`](refactor-baseline.md) |
| Remaining execution roadmap | [`senior-developer-refactor-plan.md`](senior-developer-refactor-plan.md) |

Run the architecture and supply-chain gates with:

```sh
./scripts/check-dependency-policy
./scripts/check-supply-chain
```
