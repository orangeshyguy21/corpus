# Senior developer refactor plan

Status: proposed  
Date: 2026-08-24  
Scope: the complete Corpus Rust workspace, its desktop application, MCP
servers, local-model integration, plugin boundary, tests, build system, and
maintainer documentation.

## 1. Objective

Make Corpus safer to operate, easier to diagnose, and materially easier to
change without losing the architectural boundaries that already protect
projects and research runs.

This pass will:

- establish reliable build, lint, test, and security gates;
- add real integration coverage with the local Qwen3.8 MLX model;
- preserve and strengthen project, role, plugin, and operator trust domains;
- split large files along existing domain boundaries;
- replace stringly typed schemas and command parsing with typed interfaces;
- consolidate filesystem mutation and process lifecycle behavior;
- improve structured diagnostics and failure artifacts;
- reduce unnecessary and duplicate dependencies where the result is simpler;
- move durable architecture documentation into the tracked `docs/` tree.

This is an incremental refactor, not a rewrite. Each slice must preserve a
working application and keep the relevant characterization and integration
tests green.

## 2. Non-negotiable decisions

### 2.1 Goose stays

Goose is important to the product's future and remains the embedded management
chat runtime.

This plan does **not** replace Goose, move it to a sidecar, or build a competing
Ollama agent loop. The existing `Chat`, `ChatCommand`, and `ChatEvent` boundary
continues to quarantine Goose-specific types inside the chat implementation.

The current pinned source dependency remains until the intended Goose crate is
available. At that point the source dependency should move to the crate through
a deliberate compatibility change that runs the full model-backed suite. The
migration is a packaging change, not an invitation to redesign the chat
runtime.

The current ICU and RMCP type-identity pins remain until a deliberate Goose
upgrade proves they are no longer necessary.

### 2.2 Qwen3.8 MLX inference is globally serial

The local machine can run only one model workload at a time. Every integration
test that uses Qwen3.8 MLX must execute globally serially.

`RUST_TEST_THREADS=1` is not sufficient because separate Cargo processes or
test binaries can still overlap. The integration harness must take an exclusive
cross-process file lock before loading or calling the model. The lock owns the
entire model-backed suite, records its owner, has a bounded acquisition timeout,
and is released on normal process exit.

Within the lock:

- use one shared Ollama instance;
- load only the pinned `qwen3.8:27b-mlx` model and digest;
- execute scenarios sequentially;
- do not run delegated model sessions concurrently;
- keep the model loaded between scenarios to avoid repeated load cost;
- unload it only when the suite completes or explicitly needs recovery testing.

Hermetic tests that do not use the model may still run in parallel.

### 2.3 Preserve trust-domain dependency direction

The existing high-level dependency direction is valuable:

```text
corpus-store
    └── corpus-observe
            └── corpus-core

corpus-store + corpus-observe
    └── corpus-admin
            └── corpus-admin-mcp

corpus-core + corpus-admin
    └── corpus-mcp

corpus-core + corpus-observe
    └── corpus-app
```

New crates are not a goal. A crate boundary lands only when it enforces a
runtime, trust, distribution, or build property. Most decomposition should use
modules inside the existing crates.

The dedicated administration artifact must continue to avoid the app, Goose,
research MCP, plugin execution, and run-launch dependency trees. The dependency
policy script remains a required gate.

### 2.4 Preserve current in-flight work

The worktree contains active changes in the application. Before the refactor
begins, those changes must be landed, deliberately parked, or isolated behind an
agreed interface. Mechanical moves must not silently absorb or discard them.

## 3. Current baseline

At the time of this plan the workspace contains approximately 50,400 lines of
Rust. The largest files are:

| File | Lines | Responsibilities currently concentrated there |
|---|---:|---|
| `corpus-app/src/state.rs` | 9,445 | UI projections, jobs, run lifecycle, plugin lifecycle, dispatch, reconciliation, persistence |
| `corpus-store/src/agents.rs` | 3,919 | schema, CRUD, roles, validation, permission policy, migration, rendering |
| `corpus-core/src/launch.rs` | 2,566 | launch planning, source preparation, processes, tmux, capture, export, cleanup |
| `corpus-store/src/store.rs` | 2,499 | roots, projects, missions, costs, corpus entries, filesystem mutations |
| `corpus-admin/src/lib.rs` | 2,059 | tool schemas, routing, authorization, confirmation, all handlers |
| `corpus-mcp/src/tools.rs` | 1,823 | research tool schemas, grants, routing, handlers |
| `corpus-app/src/views/projects.rs` | 1,839 | project dashboard, finding presentation, editing, actions |
| `corpus-app/src/chat/embedded.rs` | 1,715 | Goose adapter, session loop, approvals, delegation, live probes |

Observed baseline conditions:

- `cargo check --workspace --no-default-features` succeeds.
- The test suite has good isolated coverage, especially around app state,
  authorization, store contracts, and MCP profiles.
- A workspace test run reached 24 of 25 `corpus-core` unit tests before a tmux
  integration test failed in an environment that denied tmux access. Tests that
  need platform services require an explicit execution contract.
- `cargo fmt --check` is not currently green.
- strict workspace Clippy is not currently green.
- the repository has external-plugin compatibility CI but no general workspace
  format, lint, build, test, or dependency-security workflow.
- the default embedded-chat application graph is substantially larger than the
  no-default-features graph. This is expected because Goose is a product
  dependency; it makes feature-on and feature-off build gates important.

These observations are a starting point. Phase 0 records reproducible baseline
numbers on the exact integration commit before acceptance budgets are enforced.

## 4. Definition of success

The pass is complete when:

1. Formatting, strict linting, dependency policy, feature-on and feature-off
   builds, and hermetic tests are green in CI.
2. The required local-model smoke suite passes serially against a pinned
   Qwen3.8 MLX model.
3. Full nightly scenarios exercise the assembled chat, MCP, runner, tmux, and
   real plugin paths.
4. Every production incident fixed during the pass has a named regression
   scenario or focused lower-level test.
5. Application state is a composition facade over domain coordinators rather
   than a monolithic implementation.
6. Store and launch mutations use centralized, tested primitives.
7. MCP schemas, parsing, routing, authorization metadata, approval policy, and
   refresh effects cannot silently drift apart.
8. A failing system test preserves enough structured evidence to diagnose the
   failure without rerunning it interactively.
9. Durable architecture and trust-boundary documentation is tracked in
   `docs/`, while temporary investigations remain in `dev/`.

## 5. Test strategy

Integration coverage is the safety net for every later extraction. It begins
before large files are split.

### 5.1 Dedicated integration harness

Create a dedicated workspace test package:

```text
crates/corpus-integration/
  src/
    harness.rs
    artifacts.rs
    model_lock.rs
    ollama.rs
    process.rs
    preflight.rs
    assertions.rs
  tests/
    management_chat.rs
    mission_run.rs
    authorization.rs
    recovery.rs
    plugins.rs
    regressions.rs
  scenarios/
    *.yaml
```

The harness owns everything it starts and never uses the operator's real
Corpus store. Each run receives a unique private `CORPUS_HOME`, explicit
resource root, isolated process names, and bounded deadlines.

On success, scratch data is removed. On failure, the harness preserves an
artifact bundle:

```text
artifacts/<run-id>/
  manifest.json
  scenario.yaml
  events.jsonl
  transcript.md
  tool-calls.jsonl
  approvals.jsonl
  app.log
  mcp.log
  ollama.log
  process-tree.txt
  store-before/
  store-after/
  failure.txt
```

The manifest records the commit, dirty-tree status, operating system, Rust
version, Corpus binary versions, OpenCode version, tmux version, plugin
versions, Ollama version, exact model tag and digest, context size, inference
settings, and scenario deadlines.

### 5.2 Test tiers

#### Tier A: hermetic integration

Run on every pull request without model inference:

- CLI binary against an isolated real store;
- both MCP binaries over their real stdio protocol;
- real catalog, dispatch, authorization, approval, audit, and refusal behavior;
- app coordinators against deterministic runner, clock, session, and model
  adapters;
- plugin protocol negotiation against fixture processes;
- launch, stop, export, cancellation, and recovery with controlled child
  processes;
- filesystem mutation, traversal, symlink, partial-write, and stale-target
  cases.

These tests may run in parallel except for cases that deliberately mutate
process-global environment or platform resources.

#### Tier B: required Qwen3.8 MLX smoke suite

Run on the local/self-hosted model runner for changes that affect chat, tools,
prompts, MCP, state, launch, store, or permissions. It acquires the global model
lock and executes serially.

Initial scenarios:

1. Operator creates and lists a project.
2. Operator creates a role-correct agent with no tool errors.
3. Orchestrator delegates to the correct specialist and receives the result.
4. Read tools run without approval; write tools pause for approval.
5. Rejected approval causes no store mutation.
6. Mission creation and launch preserve project, mission, agent, model, and
   source-pin identity end to end.
7. A research run calls MCP and writes a valid corpus artifact tied to its run
   log.
8. Transcript, thought/tool chronology, usage, audit, and mutation events are
   durable.
9. A full curator-orchestration campaign launches child missions and delivers
   their completions back to the exact originating curator.

##### Required curator-orchestration scenario

This is a Tier B merge gate, not a nightly-only exercise. It covers the central
Corpus workflow as one assembled, model-backed scenario:

1. Start with a new isolated `CORPUS_HOME` and no pre-existing project state.
2. Create a new project through the operator surface and bind its real test
   plugin.
3. Create or materialize a curator plus the tester/researcher agents the
   campaign will use.
4. Create and launch the parent curator mission with an explicit model, source
   pins, and stable run identity.
5. Have the curator create and request launch of multiple child missions through
   its scoped tools. At least two children must be live during the campaign,
   while model inference itself remains serial under the global Qwen3.8 MLX lock.
6. Prove every child launch request and durable mission record retains the exact
   originating curator project, mission, run/session identity, agent, model,
   source pins, and request origin. No ambient selection may substitute for
   these values.
7. Let one child finish with a new corpus artifact and another finish without a
   new artifact. Record their distinct completion summaries and process state.
8. Detect each completion from durable run/session evidence and deliver a done
   notification exactly once to the originating curator conversation. The
   delivered message must identify the child and include any new artifact paths
   without relying on exact model prose.
9. Verify that a completion belonging to another curator, another parent run,
   or another project cannot be routed into this curator conversation.
10. Complete several children in the same reconciliation window and verify that
    notifications are grouped for the correct curator without losing the
    individual child identities or artifact lists.
11. Inject a notification-delivery failure. Verify the item remains retryable,
    preserves the same message identity, retries after the bounded backoff, and
    is acknowledged only after the exact curator session accepts it.
12. Replay reconciliation, duplicate filesystem events, and repeated status
    polls after acknowledgment. Verify duplicate suppression prevents a second
    notification and a second curator turn.
13. Restart the application after child completion is durable but before
    notification delivery. Recover the parent and child records, deliver the
    pending notification once, and leave no orphaned or permanently admitted
    dispatch item.
14. Exercise a child launch failure and a child session that disappears without
    a clean terminal event. Verify each produces one actionable curator
    notification and cannot be mistaken for successful completion.
15. Have the curator consume a completion notification and launch a follow-up
    mission based on it. Prove the follow-up carries the same exact-origin rules
    and forms a new, non-duplicated dispatch record.

The scenario passes only when the on-disk project, mission, dispatch, transcript,
audit, run-log, and corpus-artifact state agree with the emitted lifecycle and
delivery events. Its failure bundle must include the parent and child records,
delivery queue state, curator conversation identifiers, tool calls, model turns,
and relevant process logs.

The three existing ignored live probes in `chat/embedded.rs` should be moved
into this harness once equivalent coverage is demonstrably running. Their
behavior stays; only test ownership changes.

#### Tier C: nightly full-system suite

Run globally serially with Qwen3.8 MLX and actual platform services:

- embedded Goose runtime;
- `corpus-admin-mcp` and `corpus-mcp` binaries;
- OpenCode;
- tmux;
- Docker;
- installed CDK and Nutshell plugins;
- source preparation, sandbox calls, oracle verification, and finding writes;
- application restart and detached-session recovery;
- controlled Ollama, MCP, plugin, tmux, and app failures.

### 5.3 Model-test assertions

Do not assert exact prose. Assert observable contracts:

- tool names, arguments, order constraints, and call budgets;
- approval requests and decisions;
- resulting store records and files;
- role and project scope;
- audit and refusal entries;
- lifecycle events and durable run identity;
- transcript structure and usage records;
- bounded completion or a correctly classified timeout.

Pin model digest, context size, temperature/seed where supported, system prompt
revision, maximum turns, maximum tool calls, stream timeout, and scenario
timeout.

A retry must not hide the first failure. The harness may make one diagnostic
rerun, but the original result and artifacts remain visible and CI reports
first-attempt reliability.

## 6. Security and hardening workstream

Before moving sensitive code, write a compact threat model for:

- operator/admin process;
- project-scoped MCP process;
- embedded Goose session;
- OpenCode process and run directory;
- plugin executable and lifecycle operations;
- source cache;
- corpus store and mutable side trees;
- tmux session and transcript capture;
- model and tool output crossing process boundaries.

Audit and test the following invariants.

### 6.1 Filesystem

- All user/model/plugin-supplied paths are relative to a validated capability
  root.
- `..`, absolute paths, alternate separators, symlinks, and rename races cannot
  escape project or installation roots.
- Writes and replacements are atomic where the format represents one logical
  record.
- Exclusive creation is used where overwriting would lose evidence.
- Destructive operations bind confirmation to the inspected target state.
- Partial install, copy, move, render, and launch preparation failures leave a
  recoverable previous state.
- Cleanup errors are surfaced rather than silently converting a partially
  deleted object into success.

Create shared filesystem mutation primitives instead of reimplementing these
rules in projects, missions, agents, findings, plugin installation, and source
materialization.

Evaluate a limited `cap-std` pilot for corpus-entry and plugin-install roots.
Adopt it only if capability-relative handles materially remove path-validation
code without breaking cross-platform behavior.

### 6.2 Processes and environment

- Construct commands as program plus argument vectors whenever possible.
- Confine shell scripts and quoting to a small, directly tested adapter.
- Resolve companion executables through one policy.
- Pass a minimal explicit environment into trust-sensitive child processes.
- Never log secrets, confirmation tokens, or control passwords.
- Every child has an owner, deadline, cancellation path, and checked cleanup.
- Process-group termination and tmux cleanup failures remain visible and
  retryable.
- Project, mission, run, agent, model, and source-pin identity are exported as
  one immutable launch plan.

### 6.3 Protocols and model output

- Bound input line size, JSON depth where practical, output capture, evidence,
  and transcript growth.
- Reject malformed plugin and MCP replies cleanly.
- Treat tool arguments as untrusted typed input.
- Fail closed on unknown tools, roles, mutation classes, and refresh areas.
- Preserve the current distinction between model-visible confirmation tokens
  and real in-process operator approval.

## 7. Target module architecture

### 7.1 Application state

Keep `AppState` as the UI-facing composition root, but move behavior into
cohesive domain modules:

```text
corpus-app/src/state/
  mod.rs
  runtime.rs
  project.rs
  notices.rs
  models.rs
  plugin/
    mod.rs
    lifecycle.rs
    discovery.rs
  corpus/
    mod.rs
    findings.rs
    summary.rs
  run/
    mod.rs
    model.rs
    coordinator.rs
    launch.rs
    teardown.rs
    recovery.rs
    reconciliation.rs
  dispatch/
    mod.rs
    requests.rs
    delivery.rs
    completion.rs
```

Extraction rules:

1. Move pure types and pure functions first.
2. Move characterization tests with the behavior they protect.
3. Introduce domain state structs that own fields which must change together.
4. Express workflows as typed commands and results rather than scattered field
   mutations.
5. Keep filesystem, process, clock, session, and job adapters behind existing
   or narrower traits.
6. Make the UI read projections and submit intents; views do not start child
   processes or walk the store.
7. Preserve stale-generation rejection and exactly-once terminal job results.

The end goal is a small `state/mod.rs` facade and domain modules that can be
understood and tested independently. File length is a signal rather than a hard
rule, but new production modules should normally remain below 800-1,000 lines.

### 7.2 Store

Split `store.rs` and `agents.rs` by capability:

```text
corpus-store/src/
  store.rs                 Store roots and composition only
  project.rs
  mission.rs
  corpus_entry.rs
  cost.rs
  run_record.rs
  fs_mutation.rs
  yaml.rs
  agents/
    mod.rs
    model.rs
    repository.rs
    validation.rs
    roles.rs
    permissions.rs
    rendering.rs
    migration.rs
```

Replace long positional functions with request types such as `CreateAgent`,
`AddSubagent`, `WriteMission`, and `MoveCorpusEntry`. Validate once at the
boundary and carry validated values inward.

Keep YAML parsing behind `yaml.rs`. The current `serde_yaml` dependency is
deprecated and archived, but there is no automatic replacement. Build fixture
compatibility coverage before selecting a maintained implementation.

### 7.3 Launch and runner

Split `launch.rs` around its existing responsibilities:

```text
corpus-core/src/launch/
  mod.rs
  plan.rs
  command.rs
  process.rs
  tmux.rs
  transcript.rs
  session.rs
  cleanup.rs
```

Define a small runner interface around prepare, start, attach, observe, export,
stop, and recover. OpenCode/tmux remains the first implementation. The interface
should support the future runner work already on the roadmap without adding a
second OpenCode-specific lifecycle.

`LaunchPlan` should contain every resolved fact required to start a run. Once
constructed, launch code should not re-read ambient project state and silently
change identity.

### 7.4 Administration and research tools

Split admin and research handlers into domain modules:

```text
tools/
  registry.rs
  projects.rs
  agents.rs
  missions.rs
  corpus.rs
  models.rs
  environment.rs
  oracle.rs
  findings.rs
```

Use typed Serde argument structs plus `schemars` to generate input schemas from
the same representation used for deserialization. Each tool definition must
carry:

- name and description;
- typed input;
- handler;
- role/capability requirement;
- read, write, or destructive classification;
- confirmation policy;
- audit category;
- UI refresh/invalidation area.

Generate catalog and dispatch from the registry. Tests must prove that every
advertised tool is routable, every routable tool is advertised, every mutation
has a refresh area, destructive tools are correctly gated, and scoped schemas
do not expose a caller-selectable project.

Keep the current small synchronous MCP wire servers initially. Adopting an MCP
framework would broaden trust-critical runtime dependencies and should happen
only in response to a concrete protocol requirement.

### 7.5 Views and application shell

After state extraction stabilizes, split large views into presentation,
draft/edit state, and action modules. Consolidate repeated toast and dialog
behavior into application-level services rather than passing mutable toast
collections through many view functions.

Keep render methods cheap and deterministic. They may format cached projections
and enqueue intents but must not perform network calls, spawn subprocesses, or
walk unbounded directories.

## 8. Dependency plan

### 8.1 Keep

- Goose and its required async/MCP dependencies.
- `notify` for cross-platform event-driven reconciliation.
- `pulldown-cmark` for the custom table renderer.
- `alacritty_terminal` while `egui_term` exposes compatible terminal types.
- the Egui presentation crates that are actively used.
- `sha2`, `getrandom`, and `thiserror` for their existing security and error
  roles.

### 8.2 Simplify or align

- Align the app's direct `reqwest` with the version used by the forthcoming
  compatible Goose crate when possible. Today the workspace resolves separate
  0.12 and 0.13 lines.
- Replace direct icon decoding through `image` with
  `eframe::icon_data::from_png_bytes`; remove the direct app dependency even
  though image decoding remains transitively required by Egui.
- Benchmark Syntect's `regex-fancy` backend against the current native
  Oniguruma backend using the actual JSON and Markdown editor fixtures. Switch
  only if rendering and latency remain acceptable.
- Remove compatibility facade re-exports only after all callers depend on the
  intended narrow crate directly.

### 8.3 Add when they delete or protect meaningful code

- `schemars` for typed tool schemas.
- `clap` derive for the manually parsed CLI command tree.
- `serde_path_to_error` for actionable nested configuration errors.
- `tracing` in libraries and `tracing-subscriber` in executables/tests for
  structured run, job, model, and tool diagnostics.
- `tempfile` as a dev dependency for collision-resistant fixtures and automatic
  cleanup.
- `assert_cmd` as a dev dependency for CLI and MCP binary integration tests.
- `proptest` as a dev dependency for slugs, paths, frontmatter, permissions,
  quoting, and protocol parsers.

Do not add `serial_test` for model tests. Use the standard library's
cross-process file locking so independent Cargo invocations also serialize.

### 8.4 Supply-chain policy

Add `cargo-deny` configuration and CI checks for:

- advisories;
- accepted licenses;
- approved registry and Git sources;
- explicitly reviewed duplicate versions;
- the pinned Goose source dependency until its crate migration.

Use `--locked` for CI and release builds. Record the procedure and required
integration gates for every Goose revision or crate-version change.

## 9. Observability plan

Introduce structured spans and events with stable identifiers:

- `project`;
- `mission`;
- `run_id` and generation;
- `agent` and role;
- `job_id` and job kind;
- `chat_session` and turn;
- `tool_call` and mutation class;
- `plugin` and environment session;
- `model` and digest.

The application should produce a human-readable operator log. Integration runs
should additionally produce JSONL suitable for correlation. Sensitive values
must be redacted at the event construction boundary.

Replace ad hoc diagnostic messages gradually; do not perform a repository-wide
logging rewrite before the primary workflows have spans.

## 10. CI and quality gates

Add a primary workspace workflow with these jobs:

1. `format`: `cargo fmt --all -- --check`.
2. `lint`: strict Clippy for workspace/all targets in feature-off and default
   configurations.
3. `build-headless`: workspace build without default features.
4. `build-app`: default application and required companion binaries.
5. `test-hermetic`: unit and Tier A integration tests.
6. `dependency-policy`: existing boundary script plus `cargo-deny`.
7. `platform-tmux`: process/tmux integration tests on a prepared runner.
8. `model-qwen38`: self-hosted, globally locked, serial Tier B suite.
9. `nightly-system`: serial Tier C model and plugin matrix.

The feature-off and default builds remain separate so Goose-dependent code is
always compiled and tested without making every headless edit pay the full
dependency cost.

## 11. Execution phases

### Phase 0: stabilize the baseline

- Land or isolate current application changes.
- Select and record the integration commit.
- Make format and strict Clippy green in mechanical-only commits.
- Add primary CI and classify hermetic/platform/model tests.
- Record build time, test time, binary size, and dependency graph.

Exit: a reproducible green baseline, except explicitly separated live-service
jobs whose prerequisites are documented.

### Phase 1: integration safety net and threat model

- Create `corpus-integration`.
- Move the three existing model probes after equivalent scenarios pass.
- Add management, approval, authorization, mission, transcript, and recovery
  scenarios, including the required full curator-orchestration campaign and
  exact-origin done-notification delivery.
- Implement global Qwen3.8 MLX locking and artifact capture.
- Write the trust-boundary threat model.
- Add high-priority traversal, symlink, stale-target, malformed-reply, timeout,
  and cleanup tests.

Exit: the main user journeys are protected before structural extraction.

Status (2026-08-25): implemented. The hermetic campaign covers routing,
artifacts, retry, duplicate suppression, failure, and restart durability. The
production system campaign passed with real OpenCode/tmux processes and the
pinned Qwen3.8 MLX model, serializing curator and child turns through terminal
proofs and recovering the coordinator mid-campaign.

Issues found while closing the gate were confined to the integration seam:

- the first headless coordinator path consulted a navigation cache before it
  had selected a project, so conversation discovery never bound the curator;
- macOS exposed the temporary root as `/var/...` while OpenCode recorded its
  canonical `/private/var/...` directory, and the session ownership check
  correctly refused the unequal paths;
- tmux can become live before OpenCode creates its session record, which is a
  retryable startup state rather than a failed launch.

The harness now canonicalizes its isolated root, the headless coordinator reads
authoritative mission records and reports discovery failures, and only the
specific not-yet-created session state is retried. No production curator
routing or notification failure was reproduced after those corrections. The
restart-inclusive campaign passed in 228.54 seconds and all Phase 1 quality
gates were green.

### Phase 2: application state decomposition

- Extract pure models and projections.
- Extract plugin and corpus coordinators.
- Extract run model, coordinator, launch, teardown, and recovery.
- Extract dispatch request, delivery, and completion handling.
- Reduce `AppState` to composition, navigation-facing projections, and intent
  routing.

Exit: no workflow depends on incidental mutation ordering inside one giant
implementation block; Tier A and Tier B remain green.

Status (2026-08-25): complete. Started with behavior-free extraction of pure
state models and render projections; coordinator boundaries follow only after
the extracted types retain the existing hermetic and model-backed contracts.
The first extraction moved project, plugin, discovery, notice, mission-display,
and run-lifecycle value types into `corpus-app/src/state/models.rs`; process,
store, session, and job ownership remain in the coordinator. The next slice
moved corpus revision guards, finding projection ownership, synchronous and
background refresh scheduling, stale-result retry, and read-only corpus views
into `corpus-app/src/state/corpus.rs` while retaining the public `AppState`
surface. The third slice moved plugin discovery/probe state, lifecycle job
coordination, cancellation, durable lease drift projections, orphan cleanup,
and recovery guidance into `corpus-app/src/state/plugin.rs`. `state.rs` is now
smaller again after a fourth slice moved run backend/environment seams,
launchability guards, phase invariants, run identity and ownership,
adoption/rejection, exit polling, synchronous stop, and PTY projections into
`corpus-app/src/state/run.rs`. It now contains 7,950 lines, down from 9,531 at
the Phase 2 start, without changing its public API. A fifth slice then moved
asynchronous preparation, fresh/detached/resume launch, cancellation, durable
mission binding and adoption recovery, conversation capture, settled-turn
export, and background teardown into
`corpus-app/src/state/run/coordinator.rs`. `state.rs` now contains 6,614 lines.
A sixth slice moved external session discovery, liveness and activity polling,
scoped maintenance, restart-safe conversation recovery, repaint projection,
and activity/checkpoint predicates into `corpus-app/src/state/session.rs`.
`state.rs` now contains 6,133 lines. Curator dispatch request, activity,
delivery, completion, and retry coordination moved next into
`corpus-app/src/state/dispatch.rs`, including the headless production facade
used by the serial MLX system campaign. `state.rs` now contains 5,188 lines.
Project, agent, mission, selection, source/environment, model-discovery, and
label/sort coordination then moved into `corpus-app/src/state/resources.rs`.
`state.rs` now contains 4,296 lines, with production code ending at line 1,132
and the remaining 3,164 lines belonging to its regression module. A ninth
slice moved file invalidation, background runtime installation, scoped result
routing, stale-result retry, timeout/cancellation/failure mapping, and notice
resolution into `corpus-app/src/state/background.rs`. `state.rs` now contains
3,751 lines, with only 587 production lines before the unchanged 3,164-line
regression module. The final cleanup partitioned that regression module into
shared fixtures in `state/tests.rs` and seven workflow-focused suites for
resources, sessions, dispatch requests, corpus projection, run lifecycle,
checkpoint maintenance, and completion delivery. `state.rs` is now 589 lines
total, and the largest state test file is 893 lines. Phase 2 is complete. Both
application feature configurations, strict Clippy, the hermetic full curator
campaign, formatting, diff validation, and the dependency-policy gate are
green.

### Phase 3: store decomposition and filesystem hardening

- Introduce typed mutation requests.
- Centralize atomic and exclusive filesystem operations.
- Split project, mission, corpus entry, cost, run record, and agent modules.
- Quarantine YAML representation and build compatibility fixtures.
- Pilot capability-relative filesystem handles.

Exit: callers express intent without constructing sensitive paths or partial
write sequences themselves.

Status (2026-08-25): complete. The first slice replaced the positional
agent-creation and subagent-addition APIs with owned `CreateAgentRequest` and
`AddSubagentRequest` values. Admin handlers, app state/UI, store tests, and the
serial Qwen3.8 MLX system fixture now construct the same explicit mutation
intent. Validation and persistence behavior are unchanged. Both application
feature configurations, strict Clippy, affected crate tests, the hermetic full
curator campaign, formatting, diff validation, and the dependency-policy gate
remain green. The second slice centralized overwrite semantics in a private
`filesystem::atomic_write` primitive: unique `create_new` staging files are
written and synced in the destination directory, renamed atomically, and
cleaned on failure. Project, mission, agent, environment-session, usage,
preference, generated run/agent, and curated-entry writes now use it. Tests
cover replacement, failure preservation/cleanup, and concurrent complete-file
publication. Append-only logs, exclusive finding creation, and semantic moves
remain separate by design. No new dependency was required. The next slice
began resource-module extraction with these write semantics fixed in one
place: the `Project` record and its complete lifecycle now live in the
235-line `corpus-store/src/projects.rs`, including project-specific tree copy
and corpus-category initialization. `store.rs` retains a compatibility
re-export, preserving the root and `store::Project` paths and all `Store`
method signatures, and is now 2,413 lines rather than 2,668. All gates remain
green. The next slice moved the mission/control/request/dispatch/completion
records, legacy launch-request decoding, Markdown-frontmatter persistence,
CRUD, deletion guards, and exact-child completion delivery transitions into
the 413-line `corpus-store/src/missions.rs`. Compatibility re-exports preserve
all existing type paths and `Store` method signatures; `store.rs` is now 1,944
lines. The hermetic curator campaign remains green across origin binding,
completion, admission, acknowledgement, retry, and durable restart state.
The next slice moved `EntryAccess`, relative-path validation, canonical
containment, destination ancestor resolution, byte-counted deletion,
same-corpus moves, and atomic entry writes into the 220-line
`corpus-store/src/corpus_entries.rs`. The existing `store::EntryAccess` path
and all `Store` methods remain available. `store.rs` is now 1,703 lines. The
dedicated curation and finding-contract suites remain green for traversal,
symlink escape, immutable transcripts, recursive deletion, overwrite,
project-scope, and collision-safe finding behavior. The next slice extracted
compact usage snapshots, persistence/backfill, metadata-keyed caching, legacy
transcript parsing, tool-adjusted inference timing, and aggregation into the
406-line `corpus-store/src/accounting.rs`. Compatibility re-exports preserve
the existing root and `store::*` APIs, and `Store` retains its usage methods.
`store.rs` is now 1,287 lines. Accounting characterization, both application
feature configurations, the hermetic full curator campaign, strict Clippy,
formatting, diff validation, and the dependency-policy gate remain green. No
local model was started for this structural slice. The next slice extracted
run identity constants, the `MissionLog` projection, direct-file discovery,
timestamp parsing, ordering, and mission-session-to-agent attribution into the
106-line `corpus-store/src/run_records.rs`. Runtime run-directory provisioning
stays in `Store` because it constructs a launch workspace rather than reading
persisted transcript records. Compatibility re-exports preserve all root and
`store::*` paths, and `store.rs` is now 1,197 lines. Run-log, immutable-entry,
both application feature, hermetic curator, strict Clippy, formatting, diff,
and dependency-policy gates remain green; no local model was started. The next
Phase 3 resource is decomposition of the large agent policy and persistence
module into focused model, repository, validation, role, permission, rendering,
and migration owners. The first agent slice extracted sidecar metadata, loaded
configuration, typed mutation requests, migration results, and source pins into
the 103-line `corpus-store/src/agents/model.rs`. Public re-exports preserve all
existing agent-module and crate-root paths, including legacy serialization
defaults; `agents.rs` is now 3,876 lines, down from 3,992. Agent persistence,
clone, migration, role-ceiling, renderer, downstream, feature-off, hermetic
curator, strict Clippy, formatting, diff, and dependency-policy gates remain
green without starting a local model. Role authority and tool-catalog ownership
are the next agent slice. That slice extracted `AgentRole`, role metadata and
prompts, sandbox/management catalogs, capability checks, web/shell policy,
explicit subagent ceilings, and legacy inference into the 289-line
`corpus-store/src/agents/roles.rs`. Public paths remain unchanged; internal-only
catalog completeness and migration order did not become public API. `agents.rs`
is now 3,453 lines. Store role tests, MCP authorization and role fixtures,
application policy projections, both feature configurations, the hermetic
curator campaign, strict Clippy, formatting, diff, and dependency-policy gates
remain green without starting a local model. Permission binding, project/data
root sealing, and stored-policy tightening are the next agent slice. That slice
extracted render context, normalized actions/rules, role and stored-document
ceilings, project rebinding, task closure, relative and absolute path sealing,
immutable-run protection, canonical JSON ordering, and YAML scalar safety into
the 478-line `corpus-store/src/agents/permissions.rs`. Its API is restricted to
the parent renderer; no new public contract was introduced. `agents.rs` is now
2,986 lines. Permission characterization, downstream role fixtures, both app
feature configurations, the hermetic curator campaign, strict Clippy,
formatting, diff, and dependency-policy gates remain green without starting a
local model. Agent rendering, frontmatter assembly, prompt inlining, handle
projection, and delegation-closure orchestration are the next agent slice. That
slice extracted project-wide/additive rendering, generated-set cleanup, stable
handle derivation and collision disambiguation, flat-name claims, delegation
closure, entry-role selection, frontmatter/body assembly, corpus/source
footers, and prompt-file expansion into the 475-line
`corpus-store/src/agents/rendering.rs`. Existing `Store` methods and
`primary_handles` paths remain unchanged; helpers stay internal. `agents.rs` is
now 2,531 lines. Store renderer tests, launch materialization, downstream role
fixtures, both app feature configurations, the hermetic curator campaign,
strict Clippy, formatting, diff, and dependency-policy gates remain green
without starting a local model. The validation slice then extracted primary
configuration selection, OpenCode document and agent-map structure,
exactly-one-primary enforcement, recursive permission shapes, and prompt
references into the 151-line `corpus-store/src/agents/validation.rs`;
`agents.rs` is now 2,442 lines. The extraction exposed a containment flaw in
the previous prompt existence check: an existing `../` reference or an
in-directory symlink could resolve outside the agent directory. A shared
canonical resolver now rejects absolute and non-normal components, requires a
regular target beneath the canonical agent directory, and supplies the
canonical path used by rendering as well as save validation. Traversal and Unix
symlink regressions are covered. Store, downstream, both app feature
configurations, hermetic curator, strict Clippy, formatting, diff, and
dependency-policy gates remain green; the real Qwen3.8 MLX tests remained
ignored and no model was started. Agent repository and persistence ownership
then began with a private 159-line
`corpus-store/src/agents/repository.rs`. It now owns atomic OpenCode JSON
writes, sidecar creation and stamped mutation writes, fail-closed sidecar
reads, and recursive agent-tree copying; `agents.rs` is now 2,371 lines. The
extraction exposed that the old tree copier followed source symlinks and could
also write through a planted destination symlink. Create-from, clone, and
cross-project copy now preflight the complete source, refuse symlinks and
special files, recheck entries during the copy, and atomically claim a new
destination directory. Unix regression coverage proves both link directions
are refused, external content is unchanged, and source preflight publishes no
partial agent. Store, downstream, both app feature configurations, hermetic
curator, strict Clippy, formatting, diff, and dependency-policy gates remain
green; the real Qwen3.8 MLX tests remained ignored and no model was started.
Agent lifecycle orchestration then began by moving `Store` methods behind the
repository boundary while preserving their public paths and validation/policy
seams. The first lifecycle slice moved
sorted listing, fail-closed loading, validated atomic save, config hashing,
sidecar name/role mutations, durable delete requests, mission ownership lookup,
and guarded agent deletion into `agents/repository.rs`. The module is now 295
lines and `agents.rs` is 2,223 lines. Public `Store` method paths and behavior
remain unchanged, including preflighting every assigned mission before any
delete in the cascade. Store, downstream, both app feature configurations,
hermetic curator, strict Clippy, formatting, diff, and dependency-policy gates
remain green; the real Qwen3.8 MLX tests remained ignored and no model was
started. The next lifecycle slice moved role-based and typed creation,
inheritance, same-project clone, and cross-project copy into the repository,
sharing one validated primary-key rewrite and publication path. The module is
now 544 lines and `agents.rs` is 2,004 lines. Consolidation exposed three
behavioral gaps: clone/copy silently succeeded when a source had no valid
primary, post-copy failures left a destination directory behind, and
cross-project copy admitted a pending source agent or pending destination
project. Source validation is now mandatory before copy, a cleanup guard owns
every not-yet-persisted destination, recursive-copy failures remove the tree
they created, and deletion-pending checks apply consistently. Regressions cover
invalid inherited prompts, malformed sources, cleanup, and both pending states.
All store/downstream, app feature, hermetic curator, strict Clippy, formatting,
diff, and dependency-policy gates remain green; the real Qwen3.8 MLX tests
remained ignored and no model was started. The next lifecycle slice moved
field edits, permission patches, subagent add/remove and delegation, subagent
role assignment, and legacy role migration into the private 321-line
`agents/mutations.rs`. Shared lookup, validation, delegation, and role-ceiling
helpers keep these operations on one policy path, and `agents.rs` is now 1,695
lines. This exposed a partial-add bug: a requested incompatible subagent role
was formerly rejected only after the JSON document and delegation grant were
durable. Compatibility is now checked before either write, regression coverage
proves refusal leaves no subagent behind, and add/remove best-effort restore
the prior JSON document if sidecar persistence fails. Store, downstream, both
app feature configurations, the hermetic curator campaign, strict Clippy,
formatting, diff, and dependency-policy gates remain green; the real Qwen3.8
MLX tests remained ignored and no model was started. Production agent
ownership is now split by capability. The cleanup slice then partitioned all
29 agent characterization tests into focused role, mutation, repository, and
rendered-policy suites under `agents/tests/`, with only shared fixtures in the
test root. Test names, assertions, and Unix conditions are preserved;
`agents.rs` is now a 58-line production facade and the largest focused test
file is 553 lines. Store, downstream, both app feature configurations, the
hermetic curator campaign, strict Clippy, formatting, diff, and dependency
policy remain green; the real Qwen3.8 MLX tests remained ignored and no model
was started. Agent decomposition is complete. The next Phase 3 resource slice
moved run-workspace path composition, scoped provisioning, generated OpenCode
configuration, and symlink replacement policy into the private 290-line
`run_workspace.rs`, preserving every public `Store` method path and moving the
three workspace characterizations with their owner. `store.rs` is now 917
lines, with production code ending at line 429. The extraction exposed that
provisioning followed planted symlinks in corpus-managed parent positions and
accepted extra entries in the workspace's supposedly single-project
namespace. It now creates and verifies each managed component as a real
directory and refuses any project entry other than the selected slug. A Unix
regression proves both attacks fail before the selected link is published or
the planted parent is traversed. Store, downstream, both app feature
configurations, the hermetic curator campaign, strict Clippy, formatting,
diff, and dependency policy remain green; the real Qwen3.8 MLX tests remained
ignored and no model was started. The next Phase 3 slice extracted durable
application preferences into the 76-line `preferences.rs` and corpus
size/category/log projection into the 235-line `corpus_stats.rs`. Existing
crate-root, `store::*`, and public `Store` method paths remain compatible; the
two corpus-stat characterizations moved with their owner, and preference
round-trip and malformed-input/default coverage was added. `store.rs` is now
657 lines, with production code ending at line 267. The extraction exposed
that nested corpus symlinks were ignored while a symlink replacing the corpus
root itself was followed by `Path::is_dir`, allowing an external tree to enter
the projection. Root metadata now fails closed unless it names a real
directory, and a Unix regression covers both root and nested links. Store,
downstream, both app feature configurations, the hermetic curator campaign,
strict Clippy, formatting, diff, and dependency policy remain green; the real
Qwen3.8 MLX tests remained ignored and no model was started. The next Phase 3
cleanup slice moved the final ten `store.rs` characterizations to their
accounting, project, mission, and run-record owners. Test names, assertions,
and fixture semantics remain unchanged; each production owner now declares a
focused test module. `store.rs` is a 263-line production-only composition
facade with no embedded characterization body. Store, downstream, both app
feature configurations, the hermetic curator campaign, strict Clippy,
formatting, diff, and dependency policy remain green; the real Qwen3.8 MLX
tests remained ignored and no model was started. Phase 3 is complete: resource
persistence, agent policy, atomic filesystem mutation, run workspaces,
preferences, read projections, and their characterization suites now have
explicit owners while compatibility re-exports preserve downstream paths.
Phase 4 begins next with an immutable launch-plan boundary extracted from the
existing runner.

### Phase 4: launch decomposition

- Introduce immutable `LaunchPlan`.
- Split command, process, tmux, transcript, session, and cleanup modules.
- Tighten environment construction and executable resolution.
- Preserve cancellation, transactional adoption, stale-result rejection, and
  retryable cleanup.

Exit: runner behavior is independently testable and ready for future backend
implementations.

Status (2026-08-25): in progress. The first slice introduced the private,
immutable 224-line `corpus-core/src/launch/plan.rs`. One plan now owns every
caller-supplied launch fact, and the public positional APIs are compatibility
adapters into TUI and piped constructors accepting only `&LaunchPlan`. The
second slice extracted the private 194-line
`corpus-core/src/launch/command.rs`. Its typed backend identity projects one
owned child environment from the plan, and that same projection now drives
the TUI script, tmux session, and piped OpenCode `Command`. It also owns TUI
script rendering and shell quoting, with characterizations for exact
mission/run/environment identity and every dynamic shell value. No dependency
or long positional helper was added. The third slice extracted the private
171-line `corpus-core/src/launch/executables.rs`, which owns OpenCode and tmux
discovery, executable checks, cached tmux fallback resolution, the explicit
tmux-disable override, and tmux version capability parsing. Deterministic
characterizations lock OpenCode's PATH/user/resource precedence and supported
tmux release shapes without mutating process-global PATH or launching either
binary. The fourth slice extracted the private 215-line
`corpus-core/src/launch/tmux.rs`, which owns detached-session creation,
session environment/mouse/capture setup, external and embedded attachment,
throttled fail-live liveness checks, and race-aware checked teardown. Public
attach and teardown paths remain compatible through facade re-exports. The
extraction exposed that the former `pipe-pane` shell command did not quote its
raw-log path; paths containing spaces or apostrophes are now inert and covered
alongside detached-session argument separation and embedded attachment.
The fifth slice extracted the private 315-line
`corpus-core/src/launch/process.rs`, which owns process-group setup, piped
spawning, typed stream pumping, bounded output retention, deadline termination,
and checked tree reaping. Piped launch now opens its durable transcript before
spawn, closing the former failure window in which transcript setup could leave
a child running. Characterizations cover output caps and stream separation,
typed channel/log delivery, and timeout cleanup of the complete process group.
The sixth slice extracted the private 449-line
`corpus-core/src/launch/transcript.rs`, which owns artifact/run-stem naming,
piped transcript creation and headers, raw-tail buffering, exact OpenCode
conversation selection, bounded JSON export, durable valid-record fallback,
and public transcript projections. Public paths remain compatible through
facade re-exports. This exposed that conversation discovery still used an
unbounded `Command::output()`; it now has an owned 10-second deadline and 1 MiB
output cap. Characterizations cover oldest eligible/unclaimed selection,
truncated-export retry, capped-export fallback, buffered tail draining, and
mission-distinct artifact stems. `launch.rs` is now 1,587 lines. Model
precedence, exact Curator return identity, environment/source propagation,
transcript naming, TUI, piped, teardown, downstream, both app feature
configurations, the hermetic curator campaign, strict Clippy, formatting,
diff, and dependency policy remain green; the real Qwen3.8 MLX tests remained
ignored and no model was started. The seventh slice extracted the private
267-line `corpus-core/src/launch/session.rs`. It owns `RunSession` backend
state, typed output, launch identity and control-port projection, throttled
conversation discovery, output/exit observation, attachment, stop results,
transcript export and fallback, and checked cleanup by composing the process,
tmux, and transcript boundaries. Facade re-exports preserve the crate-root
`RunLine`, `RunSession`, and `StopOutcome` API, and `launch.rs` is now 1,256
lines. The full downstream suite, both application feature configurations,
the hermetic curator campaign, strict Clippy, formatting, diff, and dependency
policy remain green; the real Qwen3.8 MLX tests remained ignored and no model
was started. The next Phase 4 slice separates launch construction and legacy
spawn/resume routing from the facade without allowing `session.rs` to become a
replacement monolith. The eighth slice extracted the private 440-line
`corpus-core/src/launch/start.rs`. It owns interactive, resume, headless,
append, environment, and exact-run compatibility entry points; immutable-plan
routing; and TUI/piped backend construction by composing the existing focused
owners. Public `RunSession` methods remain unchanged. The split also removed
an implicit `command.rs` dependency on the facade's `LaunchPlan` re-export;
command construction now imports the plan owner directly. `launch.rs` is 839
lines, with production code ending at line 290 and characterization tests
making up the remainder. Focused launch tests, the full downstream suite, both
application feature configurations, the hermetic curator campaign, strict
Clippy, formatting, diff, and dependency policy remain green; real Qwen3.8 MLX
tests remained ignored and no model was started. The next Phase 4 slice moves
model selection, control credential, and launch identity policy behind a
focused owner, then partitions facade characterizations by capability. The
ninth slice extracted the private 221-line
`corpus-core/src/launch/policy.rs`, which owns primary-agent/explicit/registry
model precedence, loopback control-port allocation, stable per-run 0600
control credentials, and agent file/handle launch identity. Facade re-exports
preserve all public paths. `start.rs` and `command.rs` now import policy from
its owner, while launch timestamps live with construction. The model,
identity, and credential characterizations moved with policy. `launch.rs` is
622 lines with production code ending at line 119. Focused launch tests, the
full downstream suite, both application feature configurations, the hermetic
curator campaign, strict Clippy, formatting, diff, and dependency policy
remain green; real Qwen3.8 MLX tests remained ignored and no model was started.
The next Phase 4 cleanup partitions the remaining facade characterizations by
construction, observation, and compatibility behavior. The tenth slice moved
all eleven remaining tests into focused 379-line construction, 69-line
observation, and 47-line compatibility suites under `launch/tests/`; their two
shared environment/store fixtures live in a 33-line support module. Test
names, assertions, process-global locking, and the platform tmux ignore are
preserved. `launch.rs` is now a 97-line production composition and re-export
facade. Focused launch tests, the full downstream suite, both application
feature configurations, the hermetic curator campaign, strict Clippy,
formatting, diff, and dependency policy remain green; real Qwen3.8 MLX tests
remained ignored and no model was started. Phase 4 is complete: plan, command,
executable, process, tmux, transcript, session, startup, policy, and test
ownership are explicit while the public runner API remains compatible. The
next slice begins Phase 5 by inventorying the current tool argument/schema and
authorization registries before selecting the first typed boundary.

### Phase 5: typed tools and CLI

- Define typed tool arguments and generated schemas.
- Build declarative registries with authorization and refresh metadata.
- Split admin and research handler modules.
- Move CLI parsing and help generation to Clap.
- Add binary-level integration coverage.

Status (2026-08-25): complete. The opening inventory is recorded in
`docs/tool-registry-inventory.md`: 35 host-admin tools, up to 29 role-scoped
management tools, 10 research/environment tools, and management-chat exposure
currently distribute schema, dispatch, role, read/write/destructive,
confirmation, audit, and UI invalidation metadata across separate tables. The
first typed boundary is the read-only `model_list` tool. Its 66-line
`corpus-admin/src/tools/models.rs` owner derives both Serde deserialization and
the advertised schema from `ModelListArgs` with `schemars`; empty/default and
unknown-field compatibility remain unchanged, while wrong types are rejected
explicitly. `serde` and `schemars` are now direct corpus-admin dependencies,
but schemars 1.2.2 was already in the lockfile and no locked package was added.
Host and scoped catalog tests, admin and research MCP suites, the full
downstream suite, both application feature configurations, the hermetic
curator campaign, strict Clippy, formatting, diff, and dependency policy remain
green; real Qwen3.8 MLX tests remained ignored and no model was started. The
second slice introduced the private 155-line `tools/registry.rs` definition
boundary. A `ToolDefinition` now keeps tool name, description, generated input
schema, handler, and policy together; `ToolPolicy` explicitly classifies
capability, read/write/destructive behavior, confirmation, audit category, and
UI refresh area, and rejects incomplete combinations. `model_list` is the
first complete vertical migration: both catalog generation and dispatch
resolve the same definition, while its typed arguments and full handler live
in the 152-line `tools/models.rs` owner. The handwritten catalog object,
dispatch arm, and handler were removed from the 2,100-line admin facade.
Characterizations prove unique and policy-complete definitions, catalog/schema
projection, typed compatibility, and routing through the registry before any
external discovery. Focused admin/MCP tests, the full downstream suite, both
application feature configurations, the hermetic curator campaign, strict
Clippy, formatting, diff, and dependency policy remain green; real Qwen3.8 MLX
tests remained ignored and no model was started. The next slice migrates the
project tools into a focused domain module, beginning with read-only
`project_list`, then exercising write and destructive policies through the
same registry. The third slice completed that migration for `project_list`,
`project_new`, `project_clone`, `project_delete`, and `project_rebind`. Their
five typed argument contracts, generated schemas, definitions, and handlers
now live in the focused 348-line `tools/projects.rs`; the admin facade fell
from 2,100 to 1,924 lines. Read, write, and destructive project policies now
carry explicit confirmation, `projects` audit, and `projects` refresh metadata.
The delete handler still composes the existing one-shot token gate and durable
mission teardown request, and plugin rebinding still validates discovery before
mutation. Canonical `ADMIN_TOOLS` order now sorts the mixed declarative and
legacy catalog, so incremental migration cannot reorder the public tool list.
Typed optional fields retain absence/default and unknown-field compatibility,
while present wrong types now fail before store or plugin work. Focused policy,
schema, host/scoped catalog, destructive-confirmation, audit, and curator tests;
the full downstream suite; both app feature configurations; the hermetic
curator campaign; strict Clippy; formatting; diff; and dependency policy remain
green. Real Qwen3.8 MLX tests remained ignored and no model was started. The
next slice introduces the agent tool domain with typed `agent_list` and
`agent_get` read boundaries before migrating its larger mutation surface. The
fourth slice moved both reads into the focused 130-line `tools/agents.rs`.
Their typed host arguments, generated schemas, definitions, read-only policy,
listing projection, and pretty-document handler now share one owner; the
handwritten entries, routing arms, and handlers left the facade, which is now
1,861 lines. The scoped-catalog characterization proves only the
launcher-proven `project` argument is removed: `agent_get` continues to require
`agent`, while host schemas continue to require both scope and resource
identity. Registry-owned generic schema generation and typed deserialization
now serve model, project, and agent domains instead of being repeated in each
module. Missing or wrong required values fail before store access; unknown
future fields remain accepted. Focused host/scoped catalog and routing suites,
the full downstream suite, both app feature configurations, the hermetic
curator campaign, strict Clippy, formatting, diff, and dependency policy remain
green. Real Qwen3.8 MLX tests remained ignored and no model was started. The
next agent slice migrates validated save/create and same-/cross-project
clone/copy operations before the granular mutations and destructive delete.
The fifth slice completed those four persistence operations in the now
346-line `tools/agents.rs`; the admin facade is down from 1,861 to 1,722 lines.
`agent_new` has an executable typed role enum matching its generated schema,
retains inherited-agent and destination defaults, and still delegates final
policy validation to the store's structured `CreateAgentRequest` boundary.
`agent_save` now accepts only the object shape it has always advertised before
passing that document to the core validator. Same-project clone and
cross-project copy retain their existing repository guards and default copy
slug. All four definitions carry explicit `agents` audit and refresh metadata
as unconfirmed writes. Characterizations cover required fields, role values,
document shape, future-field compatibility, copy defaults, and policy. Existing
valid/invalid save round-trips, scoped authorization, mutation audit, agent
repository, full downstream, both app feature configurations, hermetic curator,
strict Clippy, formatting, diff, and dependency-policy gates remain green.
Real Qwen3.8 MLX tests remained ignored and no model was started. The next
slice migrates granular field, role, permission, and subagent mutations; agent
deletion remains a separate destructive slice. The sixth slice migrated
`agent_set`, `agent_set_role`, `agent_set_permission`,
`agent_subagent_add`, and `agent_subagent_remove` into the now 638-line agent
domain, reducing the facade from 1,722 to 1,519 lines. Generated enums now
drive the four allowed scalar fields and four role ceilings; permission input
is typed as an object; and the required field value remains arbitrary JSON so
`null` still clears it. Empty optional subagent names continue to normalize to
the primary entry exactly as before. All handlers compose the existing store
mutation boundary, preserving surgical validation, role ceilings, delegation
wiring, preflight compatibility, and rollback behavior. The five definitions
are explicitly unconfirmed `agents`-audited, `agents`-refreshing writes.
Characterizations cover null clearing, empty-subagent compatibility, invalid
field/role/patch types, and policy. Focused store mutation and role suites,
scoped authorization and audit, the full downstream suite, both app feature
configurations, hermetic curator, strict Clippy, formatting, diff, and
dependency-policy gates remain green. Real Qwen3.8 MLX tests remained ignored
and no model was started. The next slice moves confirmation-gated agent
deletion and its delegation-consequence projection into a focused
`agents/delete.rs` owner rather than growing the domain facade further. The
seventh slice completed that 210-line owner. Typed `AgentDeleteArgs`, generated
schema, destructive policy, dry-run consequence projection, one-shot token
execution, mission cascade reporting, durable teardown requests, and dangling
delegation detection now live together; the admin facade fell from 1,519 to
1,385 lines. The definition explicitly requires token confirmation and carries
`agents` audit and refresh metadata. A present non-string token is rejected
before preview or mutation. Existing scoped curator coverage still proves the
dry run and confirmed result name exactly the assigned mission, delete it with
the agent, and preserve unrelated missions; live mission guards still choose a
durable app teardown request. Focused consequence helpers distinguish allowed
delegations from explicit denial. Host/scoped catalog, confirmation, audit,
agent repository, app lifecycle, full downstream, both feature configurations,
hermetic curator, strict Clippy, formatting, diff, and dependency-policy gates
remain green. Real Qwen3.8 MLX tests remained ignored and no model was started.
Agent tool migration is behavior-complete. The next cleanup partitions the
remaining 642-line agent domain into focused read, persistence, and mutation
owners before beginning typed mission tools. The eighth slice completed that
cleanup without changing the registry or public tool behavior. The former
642-line domain is now a 12-line composition facade over a 126-line read owner,
204-line persistence owner, 273-line granular-mutation owner, existing 210-line
deletion owner, and 35-line shared role/write-policy contract. Schema and policy
characterizations moved with the behavior they protect, and the shared role
enum remains the single executable contract for create, role, and subagent
operations. The focused admin and MCP suites, full downstream suite, both app
feature configurations, hermetic curator campaign, strict Clippy, formatting,
diff, and dependency-policy gates remain green. Real Qwen3.8 MLX tests remained
ignored and no model was started. The next slice begins typed mission reads,
starting with `mission_list`, `mission_get`, and `mission_status`; the blocking
`mission_await` boundary remains separate so its timing semantics can be
characterized explicitly. The ninth slice completed those three immediate
reads in a focused 193-line `tools/missions.rs` owner, reducing the admin facade
from 1,385 to 1,278 lines. Typed required project/mission identities and the
optional status selector now generate the advertised schemas and drive the
handlers; a present non-string optional mission is rejected instead of silently
selecting every mission. The registry marks all three as unconfirmed,
unaudited, non-refreshing reads. Mission listing, full brief/pin projection, and
the running/waiting/idle observation semantics are unchanged, including one
shared live-session snapshot per call. Generated-contract and policy
characterizations join the existing scoped curator status coverage. Focused
admin/MCP, full downstream, both app feature configurations, hermetic curator,
strict Clippy, formatting, diff, and dependency-policy gates remain green.
Real Qwen3.8 MLX tests remained ignored and no model was started. The next
slice isolates and types `mission_await`, preserving its operator-only catalog
boundary and adding deterministic coverage for timeout clamping and snapshot
selection before moving mission writes. The tenth slice completed that
isolation in a focused 285-line `tools/missions/wait.rs` owner and reduced the
admin facade from 1,278 to 1,029 lines. It also fixed a catalog defect found
during migration: the description and handler supported `timeout_secs`, but
the handwritten schema did not advertise it. `MissionAwaitArgs` now generates
one executable contract for project scope, optional exact-mission selection,
and the optional unsigned timeout; present wrong types fail before the blocking
boundary. Pure tests cover the 45-second default, 1/90-second clamps,
all-versus-one selection, unchanged snapshots, activity transitions, and new
corpus output without sleeping or starting tmux. The actual handler retains
its two-second serial poll, bounded single-threaded call, record reload,
fallback snapshot, and timeout wording. Its registry policy is read-only, and
existing scoped coverage continues to prove project agents never receive the
operator diagnostic. Focused admin/MCP, full downstream, both app feature
configurations, hermetic curator, strict Clippy, formatting, diff, and
dependency-policy gates remain green. Real Qwen3.8 MLX tests remained ignored
and no model was started. The next slice migrates mission creation and settings
(`mission_new`, `mission_set_budget`, and `mission_set_pins`) before handling
origin-sensitive launch and confirmation-gated deletion separately. The
eleventh slice completed those three writes in a focused 230-line
`tools/missions/persistence.rs` owner, reducing the admin facade from 1,029 to
873 lines. Typed creation keeps required identity/brief fields, optional budget
and normalized display name, and a `BTreeMap<String, String>` pin overlay;
project pins still load first and explicit mission pins still win. Pin
validation remains author-time and fails open when a plugin cannot enumerate
sources. Budget replacement and full pin-map replacement retain their existing
store update and response behavior. Generated contracts now reject wrong
optional string types, non-object pin maps, and non-string pin revisions that
the handwritten schema did not consistently enforce. All three definitions are
unconfirmed, `missions`-audited, `missions`-refreshing writes. Focused contract
and policy tests join existing curator characterizations for inheritance,
overrides, validation, scoping, and audit. Focused admin/MCP, full downstream,
both app feature configurations, hermetic curator, strict Clippy, formatting,
diff, and dependency-policy gates remain green. Real Qwen3.8 MLX tests remained
ignored and no model was started. The next slice migrates origin-sensitive
`mission_launch`, preserving its launcher-proven return address and pending
deletion guards before destructive mission deletion moves separately. The
twelfth slice completed that 152-line `tools/missions/launch.rs` owner and
reduced the admin facade from 873 to 803 lines. `MissionLaunchArgs` generates
only project and mission identity; model-supplied `requested_by` remains an
ignored compatibility field and can never enter the durable request. The
handler receives launcher-proven origin solely through the registry invocation
context, rejects cross-project origin, and stores the exact mission/run return
address. Existing project, agent, and mission pending-deletion guards retain
their order, an existing request remains idempotent, and the app remains the
only component that consumes the flag and starts a run. The definition is an
unconfirmed, `missions`-audited, `missions`-refreshing write. Focused tests
cover generated schema, origin exclusion/matching, and policy; scoped curator
coverage continues to prove scope injection, spoof resistance, distinct
simultaneous return addresses, idempotency, brief preservation, and fail-closed
partial identity. Focused admin/MCP, full downstream, both app feature
configurations, hermetic curator, strict Clippy, formatting, diff, and
dependency-policy gates remain green. Real Qwen3.8 MLX tests remained ignored
and no model was started. The next slice moves confirmation-gated
`mission_delete` into its own lifecycle owner, preserving immediate safe
deletion versus durable app teardown requests. The thirteenth slice completed
that focused 138-line `tools/missions/delete.rs` owner and reduced the admin
facade from 803 to 735 lines. Typed `MissionDeleteArgs` now generates the
project/mission/token contract; a present non-string token fails before dry run
or mutation. The definition is confirmation-token gated, destructive,
`missions`-audited, and `missions`-refreshing. Dry runs still load the record,
sample live sessions once, and report agent, liveness, and optional budget
before minting a one-shot operation/target-bound token. Confirmed deletion
still removes immediately only when every store lifecycle guard allows it;
otherwise it clears a pending launch, preserves run/environment identity,
idempotently records a durable delete request, and leaves teardown to the app.
Focused schema/policy and summary tests join existing token, scoped curator,
store guard, live teardown, retry, and app lifecycle coverage. Focused
admin/MCP, full downstream, both app feature configurations, hermetic curator,
strict Clippy, formatting, diff, and dependency-policy gates remain green.
Real Qwen3.8 MLX tests remained ignored and no model was started. Mission tool
migration is behavior-complete. The next cleanup moves the remaining read and
shared live/status presentation helpers behind focused owners so
`tools/missions.rs` becomes a composition facade before typed corpus tools.
The fourteenth slice completed that behavior-neutral cleanup. The former
225-line mission domain is now a 14-line composition facade over focused read,
wait, persistence, launch, delete, and shared presentation owners. Typed
`mission_list`, `mission_get`, and `mission_status` contracts and their tests
moved with the 192-line read owner; shared live/status labeling moved into a
48-line private module with direct compact idle-duration boundary coverage.
The admin facade fell from 735 to 712 lines. Catalog order, dispatch,
authorization, approval, audit, invalidation, scoped behavior, lifecycle
routing, and output remain unchanged. Focused admin/MCP, full downstream, both
app feature configurations, hermetic curator, strict Clippy, formatting, diff,
and dependency-policy gates remain green. Real Qwen3.8 MLX tests remained
ignored and no model was started. The next slice starts typed corpus reads with
`corpus_stats`, `corpus_list`, and `corpus_read`; structured finding reads stay
separate so each contract can be characterized at its own boundary. The
fifteenth slice completed those three reads behind a 5-line corpus composition
facade and focused 251-line read owner, reducing the admin facade from 712 to
619 lines. Their handwritten catalog entries and untyped dispatch arms are now
generated definitions with explicit read-only, unconfirmed, unaudited, and
non-refreshing policy. Typed arguments reject wrong project, category, and path
types before filesystem observation while retaining unknown-field
compatibility and the existing category allowlist. Direct tests now cover
generated schemas, sorted category output, exact body reads, corpus/log stats
separation, and invalid-category reporting. Scoped project injection, path
confinement, catalog order, output, and authorization remain unchanged.
Focused admin/MCP, full downstream, both app feature configurations, hermetic
curator, strict Clippy, formatting, diff, and dependency-policy gates remain
green. Real Qwen3.8 MLX tests remained ignored and no model was started. The
next slice isolates typed structured `finding_list`, including its severity,
unrated, text, sort, and limit query contract. The sixteenth slice completed
that focused 298-line owner and reduced the admin facade from 619 to 505 lines.
The handwritten union schema and untyped handler are now one generated
contract with explicit immediate, unconfirmed, unaudited, and non-refreshing
read policy. String-or-set severity filtering retains duplicate elimination
and the existing actionable invalid-severity errors; default inclusion of
unrated findings, newest-first ordering, text matching, severity ordering, and
positive result limits remain intact. Typed parsing now rejects malformed
optional `include_unrated`, `text`, and `sort` values that the old handler
silently treated as defaults. Zero limits are rejected before finding
discovery, and unknown fields remain accepted for compatibility. Existing
recursive structured-output, metadata-warning, filter, scoped-project, and
read-only audit coverage remains green alongside focused schema/default/error
tests. Full downstream, both app feature configurations, hermetic curator,
strict Clippy, formatting, diff, and dependency-policy gates remain green.
Real Qwen3.8 MLX tests remained ignored and no model was started. The next
slice isolates confirmation-gated `corpus_wipe`, preserving its project/agent
survival contract and generation advance before typed entry lifecycle tools.
The seventeenth slice completed that focused 153-line owner and reduced the
admin facade from 505 to 468 lines. `CorpusWipeArgs` now generates the required
project and optional token contract; a present non-string token fails before
corpus observation instead of silently starting another dry run. The
definition is explicitly destructive, token-confirmed, `corpus`-audited, and
`corpus`-refreshing. Dry-run file/byte and next-generation reporting,
project-bound one-shot tokens, confirmed output, and generation advancement
remain unchanged. Direct coverage proves contents are removed while the
project and its agents survive, joining existing single-use/op-scoped token,
scoped Super authority, app revision/invalidation, and store wipe coverage.
Focused admin/MCP, full downstream, both app feature configurations, hermetic
curator, strict Clippy, formatting, diff, and dependency-policy gates remain
green. Real Qwen3.8 MLX tests remained ignored and no model was started. The
next slice isolates confirmation-gated `entry_delete` with its recursive-tree
guard and target-state fingerprint binding. The eighteenth slice completed a
focused 219-line delete owner behind a 5-line entry lifecycle facade and
reduced the admin facade from 468 to 335 lines. `EntryDeleteArgs` now generates
the project/path, default-false recursive flag, and optional token contract;
present non-boolean recursive flags and non-string tokens fail before path
resolution instead of being treated as omitted. The definition is explicitly
destructive, token-confirmed, `corpus`-audited, and `corpus`-refreshing. The
security sequence remains unchanged: canonical confined resolution, sorted
metadata-only tree preview, operation/path/mode/fingerprint-bound confirmation,
then store-side confined deletion. Direct tests cover deterministic counts and
fingerprints plus mutation sensitivity. Existing stale-token, recursive-tree,
runs immutability, symlink/traversal, scoped curator, audit, full downstream,
both app feature configurations, hermetic curator, strict Clippy, formatting,
diff, and dependency-policy gates remain green. Real Qwen3.8 MLX tests remained
ignored and no model was started. The next slice isolates typed `entry_move`,
including overwrite semantics and same-corpus source/destination confinement.
The nineteenth slice completed that focused 168-line owner inside the entry
lifecycle namespace and reduced the admin facade from 335 to 306 lines.
`EntryMoveArgs` now generates the project/source/destination and default-false
overwrite contract; a present non-boolean overwrite value fails before path
resolution instead of silently disabling replacement. The definition is an
unconfirmed, `corpus`-audited, `corpus`-refreshing write. Exact output, missing
destination-parent creation, collision refusal, and explicit replacement now
have direct tool-level coverage. Store-owned same-project confinement,
source/destination canonicalization, bare-category and runs immutability,
symlink/traversal resistance, and no-clobber defaults remain unchanged.
Focused admin/MCP, full downstream, both app feature configurations, hermetic
curator, strict Clippy, formatting, diff, and dependency-policy gates remain
green. Real Qwen3.8 MLX tests remained ignored and no model was started. The
next slice isolates typed `entry_write`, completing declarative ownership of
the admin catalog and dispatch table.
The twentieth slice completed that focused 140-line owner inside the entry
lifecycle namespace and reduced the admin facade from 306 to 259 lines.
`EntryWriteArgs` now generates the required project/path/content contract;
wrong-typed values fail before store access while unknown fields remain
accepted for forward compatibility. The definition is an unconfirmed,
`corpus`-audited, `corpus`-refreshing write. Direct tool coverage proves exact
UTF-8 byte reporting, missing-parent creation, and replacement in place.
Store-owned category validation, canonical confinement, immutable `runs/`,
atomic replacement, traversal/symlink resistance, scoped project injection,
and curator audit behavior remain unchanged. All 35 catalog entries and
dispatch routes now come from the same typed registry; the last handwritten
catalog element, legacy string extractor, handler, and fallback match are
gone. Focused admin/MCP, full downstream, both app feature configurations,
hermetic curator, strict Clippy, formatting, diff, and dependency-policy gates
remain green. Real Qwen3.8 MLX tests remained ignored and no model was started.
The next cleanup slice moves shared project loading and confirmation-token
infrastructure out of the public admin facade, leaving it responsible for
state, catalog projection, and dispatch only.
The twenty-first slice completed that cleanup. The admin facade fell from 259
to 170 lines and now owns only public server/context state, the canonical tool
sets, catalog projection, and dispatch. Shared project loading and Unix-time
adaptation live in a private 21-line common module; the complete one-shot token
protocol and its rationale live in a focused 173-line confirmation owner.
`PendingConfirm` remains publicly nameable so the scoped MCP adapter can hold
and lend its map, but its operation, target, and expiry fields are now opaque;
only the confirmation module can mint, interpret, or consume them. Direct
tests now prove the exact dry-run/confirmed output, success replay rejection,
and consumption on target mismatch, expiry, and failed mutation. Existing
five-operation confirmation classification, 60-second TTL, target bindings,
destructive authorization, lifecycle routing, and curator behavior remain
unchanged. Focused admin/MCP, full downstream, both app feature
configurations, hermetic curator, strict Clippy, formatting, diff, and
dependency-policy gates remain green. Real Qwen3.8 MLX tests remained ignored
and no model was started. The next Phase 5 slice establishes the typed Clap
command boundary for the 552-line CLI entry point, beginning with top-level
dispatch and `run` parsing while preserving current exit and help behavior.
The twenty-second slice completed that boundary in a focused 181-line
`corpus-cli/src/cli.rs` owner and reduced the entry point from 552 to 462
lines. Clap 4.6.6 now derives the top-level command tree, typed `RunArgs`,
option validation, and long help; it was already present in `Cargo.lock`, so
making it a direct CLI dependency added no locked package. The manual usage
document, top-level string match, and hand-rolled `run` argument loop are gone.
Agent/model/research fields and the non-empty multiword mission are validated
before store, source, environment, or model work. Unconverted plugin and store
domains pass their exact trailing argv through the typed top-level boundary so
the migration remains behaviorally incremental. Bare invocation still prints
help successfully; help and errors consistently name the installed `corpus`
binary rather than the `corpus-cli` package; unknown commands retain the
`corpus: error:` prefix and failure exit. Parser tests cover option placement,
required fields, typo rejection, help, and passthrough fidelity. New
process-level tests cover bare/flag help, executable naming, unknown routing,
and malformed-run rejection before project/model access; the existing
out-of-workspace model resource test remains green. Focused CLI, full
downstream, both app feature configurations, hermetic curator, strict Clippy,
formatting, diff, and dependency-policy gates remain green. Real Qwen3.8 MLX
tests remained ignored and no model was started. The next CLI slice types the
`models` and `plugin` command trees, removing their remaining manual nested
dispatch while preserving lifecycle deadlines, progress, raw JSON calls, and
installation verification.
The twenty-third slice completed those trees. `ModelsCommand` and all nine
`PluginCommand` variants now derive required arguments, nested help, and extra
argument rejection from Clap before registry, filesystem, JSON, or process
work. Their behavior moved from the entry point into focused 31-line model and
182-line plugin command owners behind a 4-line composition module, reducing
`main.rs` from 462 to 283 lines. Plugin catalog validation still precedes every
operation; installed-plugin lookup still verifies the bundle before spawn;
setup retains its 30-minute deadline while doctor/status/stop retain two
minutes; V1 probe still performs hello plus lifecycle status with ten seconds;
legacy probe output, progress reporting, atomic install/select, raw V1/legacy
calls, optional typed JSON parameters, and pretty output are unchanged. Pure
tests characterize deadline selection and JSON parsing. Parser and process
tests cover typed call/setup/model forms, required and excess argument
rejection before discovery, and generated nested help. The out-of-workspace
model resource binary test remains green. Focused CLI, full downstream, both
app feature configurations, hermetic curator, strict Clippy, formatting, diff,
and dependency-policy gates remain green. Real Qwen3.8 MLX tests remained
ignored and no model was started. The next CLI slice types and extracts the
project command tree, including clone options and destructive wipe/delete
forms, before proceeding through agent, mission, finding, audit, and refusal
domains.
The twenty-fourth slice completed that project boundary. All six
`ProjectCommand` variants now derive required values, the `cdk-regtest` new
project default, clone destination/name/corpus options, rebind plugin input,
nested help, and excess-argument rejection from Clap. The handlers moved into
a focused 102-line project owner, reducing the transitional store-admin module
from 763 to 635 lines. List formatting, name fallback, clone semantics, clean
deletion, durable deletion requests when any mission still owns live teardown
identity, corpus wipe generation advancement, rebind output, and store-owned
slug/path validation remain unchanged. Parser tests cover defaults, complete
clone projection, destructive targets, and required options. A new isolated
binary campaign creates, lists, clones with corpus/name flags, rebinds, wipes,
deletes, and proves the final store projection; a separate process test proves
missing `--to` fails before the data root exists. Existing store/app lifecycle
tests continue to cover the live-mission deletion-request branch. Focused CLI,
full downstream, both app feature configurations, hermetic curator, strict
Clippy, formatting, diff, and dependency-policy gates remain green. Real
Qwen3.8 MLX tests remained ignored and no model was started. The next CLI slice
types and extracts the agent tree, including role value parsing, dry-run role
migration, clone destination, and lifecycle-aware deletion.
The twenty-fifth slice completed that agent boundary. All six
`AgentCommand` variants now derive required project/agent targets, the
`researcher` creation default, clone destination, optional role get/set value,
and migration apply mode from Clap. Role validation delegates to the core
`AgentRole::parse` and `AgentRole::names` authority rather than introducing a
second CLI role model. The handlers moved into a focused 156-line agent owner,
reducing the transitional store-admin module from 635 to 456 lines. List and
role formatting, cloning, clean deletion, durable deletion requests while an
assigned mission still owns teardown identity, and dry-run-by-default role
migration remain unchanged. Parser tests cover defaults, clone and migration
projection, invalid roles, and the required clone destination. A new isolated
binary campaign creates, lists, reads and changes a role, clones, previews role
migration, deletes, and proves the final store projection; a separate process
test proves an invalid role fails before the data root exists. Existing
live-mission lifecycle coverage, focused CLI, full downstream, both app feature
configurations, hermetic curator, strict Clippy, formatting, diff, and
dependency-policy gates remain green. Real Qwen3.8 MLX tests remained ignored
and no model was started. The next CLI slice types and extracts the mission
tree, including its required agent, repeatable pins, multiword brief,
default/project pin precedence, and lifecycle-aware deletion.
The twenty-sixth slice completed that mission boundary. All three
`MissionCommand` variants now derive required project/mission targets, the
required agent, optional budget, repeatable typed `source=revision` overrides,
required multiword brief, nested help, and excess-argument rejection from
Clap. Malformed pin syntax and missing authoring inputs now fail before store
or plugin discovery. The handlers moved into a focused 191-line mission owner,
reducing the transitional store-admin module from 456 to 311 lines. A direct
precedence test fixes the source selection order as plugin defaults, then
stored project pins, then per-mission overrides. Existing structural revision
validation, list formatting, Markdown brief storage, clean deletion, and
durable deletion requests for live run identities remain unchanged. Parser
tests cover repeated pins interleaved with brief words, required inputs, typed
deletion, and malformed pin values. A new isolated binary campaign creates a
project and agent, creates and lists a budgeted multi-pin mission, verifies its
stored multiword brief, deletes the clean mission, then proves a mission with a
live session is retained with `delete_requested`; a separate process test
proves malformed pins fail before the data root exists. Existing lifecycle,
focused CLI, full downstream, both app feature configurations, hermetic
curator, strict Clippy, formatting, diff, and dependency-policy gates remain
green. Real Qwen3.8 MLX tests remained ignored and no model was started. The
next CLI slice types and extracts finding list/show, including repeatable and
comma-separated severity filters, unrated inclusion, typed sorting, positive
limits, text search, and confined finding paths.
The twenty-seventh slice completed that finding boundary. `FindingCommand`
now derives required project/path targets, repeatable and comma-separated core
`FindingSeverity` values, unrated exclusion, optional text search,
newest/severity sorting, and positive limits from Clap. Invalid severity, sort,
limit, missing path, and excess argument shapes fail before store discovery.
The projection and exact-read handlers moved into a focused 97-line finding
owner, reducing the transitional store-admin module from 311 to 144 lines.
Severity parsing and `.md`/`findings/` path confinement continue to use the
core authorities. Tabular formatting, tolerant unrated projection, warning
rendering, empty-result output, in-memory filtering, and byte-exact show output
remain unchanged. Parser tests cover the complete query and invalid values; a
direct projection test fixes severity deduplication and unrated mapping. A new
isolated binary campaign materializes rated and unrated Markdown findings,
lists them, exercises combined repeated/comma filters, text, severity sorting,
and limits, verifies empty results and exact reads, and proves traversal is
refused. A separate process test proves an invalid severity fails before the
data root exists. Existing finding-contract, confinement, focused CLI, full
downstream, both app feature configurations, hermetic curator, strict Clippy,
formatting, diff, and dependency-policy gates remain green. Real Qwen3.8 MLX
tests remained ignored and no model was started. The next CLI slice types and
extracts the audit and refusal log readers, including tail defaults, typed
refusal gates, empty-log diagnostics, and final removal of the transitional
store-admin module.
The twenty-eighth slice completed those operator-log boundaries. `AuditArgs`
and `RefusalsArgs` now derive required project identity, the shared 50-record
tail default, and numeric validation from Clap; refusal filtering additionally
parses directly to the core seven-variant `refusal::Gate` authority. Unknown
gates, non-numeric tails, missing projects, and excess arguments fail before
store observation. Audit and refusal rendering moved into a focused 88-line
operator-log owner, the 144-line transitional store-admin module and its
passthrough parser type were deleted, and every headless command domain is now
routed through the generated typed boundary. Existing tail-before-filter
semantics, oldest-first output, three-line detail cap, audit outcome/target
formatting, refusal role/run/argument diagnostics, and the positive empty-log
explanation remain unchanged. Parser tests cover defaults, explicit tail/gate,
required values, and invalid types. A new isolated binary campaign covers
empty audit/refusal diagnostics, audit tailing and detail truncation, typed
refusal filtering, role/run/argument rendering, and pre-store invalid-gate
rejection. Existing append-only/out-of-project log coverage, focused CLI, full
downstream, both app feature configurations, hermetic curator, strict Clippy,
formatting, diff, and dependency-policy gates remain green. Real Qwen3.8 MLX
tests remained ignored and no model was started. Phase 5 is complete: all 35
admin tools share generated typed definitions and declarative policy metadata,
the CLI is fully typed with binary campaigns, and no handwritten catalog,
dispatch, or argument-parser fallback remains. The next slice begins Phase 6
with a refreshed view-size, observability, and dependency inventory before the
first UI extraction or dependency removal.

Exit: catalogs, dispatch, authorization, approval, audit, and invalidation
cannot drift silently.

### Phase 6: views, observability, and dependency cleanup

Status (2026-08-25): in progress.

- Split large views after their state APIs settle.
- Land structured tracing across critical workflows.
- Align duplicate dependency lines where compatibility permits.
- Remove the direct image decoder dependency.
- Evaluate Syntect backend and YAML replacement.
- Add supply-chain policy.

The Phase 6 entry slice recorded a refreshed
[`phase-6-inventory.md`](phase-6-inventory.md) before changing a boundary.
The largest UI owners are `views/projects.rs` (1,821 lines),
`views/agents.rs` (1,351), `sidebar.rs` (1,281), and `chat/panel.rs` (1,223).
Production has no structured tracing subscriber yet; lifecycle and delivery
coordination are the first instrumentation seams, with stable project,
mission, run/session, operation, generation, elapsed, outcome, and retryability
fields. Dependency inspection also isolated the Syntect Oniguruma backend and
the deprecated YAML boundary as separate compatibility campaigns. Goose and
its pins remain untouched.

The first cleanup slice removed Corpus's direct `image 0.25` workspace and app
dependencies. The sole consumer now uses eframe 0.31.1's exact
`icon_data::from_png_bytes` adapter, and a regression test fixes the embedded
icon at 250 by 250 pixels with a complete RGBA buffer. Refreshing the lockfile
offline removed 41 package entries and 454 lines from the decoder/build graph.
Image versions still remain transitively through UI dependencies and Goose;
the result is removal of the redundant direct edge and default-feature request,
not a claim that every image crate disappeared. Strict Clippy, all selected
workspace tests, both default and no-default-feature app gates, dependency
policy, formatting, and diff checks pass. The real Qwen3.8 MLX tests remained
ignored and no model was started. The next slice extracts the finding-summary
responsibility from `views/projects.rs` behind its existing projection tests.

The second slice completed that extraction. The project dashboard now depends
on an opaque `FindingSummary` component with only construction, visibility,
and rendering operations; count storage and loading, ready, failure, and
last-good states remain private to the component. Severity projection,
zero-count omission, responsive tile sizing, status rendering, and all four
existing regression tests moved together into a focused 250-line owner.
`views/projects.rs` fell from 1,821 to 1,591 lines and no longer imports
finding discovery or severity types directly. Strict Clippy, full downstream
tests including the hermetic curator campaign, both app feature configurations,
dependency policy, formatting, and diff checks pass. The real Qwen3.8 MLX
tests remained ignored and no model was started. The next slice defines the
structured tracing contract and instruments its first lifecycle boundary.

The third slice established that contract without putting telemetry in paint
code. Corpus now has one typed lifecycle event owner over `tracing 0.1.44` with
the stable `corpus.lifecycle` target and `lifecycle.operation` event name.
Every event carries project, mission, run-session storage identity, operation,
generation, elapsed milliseconds, outcome, retryability, and error fields.
The asynchronous launch-adoption boundary emits exactly one terminal event on
success or failure; retryability is projected from the authoritative retained
`RunPhase`, rather than guessed from error text. A test-only
`tracing-subscriber 0.3.23` capture layer locks the complete success contract,
and the deletion-during-adoption lifecycle test locks the real failure event
and cleanup behavior. Both packages were already resolved in the lockfile, so
the slice adds direct responsibility edges without a new package or Goose
change. Production subscriber installation remains intentionally separate
from the event contract. Strict Clippy, full downstream tests including the
hermetic curator campaign, both app feature configurations, dependency policy,
formatting, and diff checks pass. The real Qwen3.8 MLX probes remained ignored
and no model was started. The next slice installs a bounded local subscriber at
the executable boundary and proves startup remains non-fatal when the
diagnostic sink is unavailable.

The fourth slice installed that subscriber before application state or chat
initialization. Only the `corpus.lifecycle` target is enabled; events are
flattened JSONL under `<CORPUS_HOME>/var/diagnostics`, without ANSI or span
payload noise. The existing `tracing-appender 0.2.5` stack supplies a bounded
non-blocking channel, daily rotation, and retention pruning at eight matching
files. Its guard lives for the full GUI process so queued events flush on
shutdown. Sink creation and global-subscriber conflicts return contextual
errors; the executable prints one warning and continues startup. Tests prove
retention removes only matching diagnostic files, preserves unrelated entries,
reports an unusable path without panic, and converts subscriber failure into a
non-fatal startup result. `tracing-appender` and production
`tracing-subscriber` reuse packages already in the lockfile, and Goose remains
untouched. Strict Clippy, full downstream tests including the hermetic curator
campaign, both app feature configurations, dependency policy, formatting, and
diff checks pass. Real Qwen3.8 MLX tests stayed ignored and no model was
started. The next observability slice instruments curator completion delivery
with message identity and terminal retry state.

The fifth slice completed the second critical observability seam. Terminal
curator completion delivery now emits `corpus.delivery` /
`delivery.operation` with launcher-proven project, parent mission and run,
deterministic message id, delivery attempt, grouped child count, elapsed
milliseconds, outcome, terminal state, retryability, and error. Admission,
pending, and active polling remain quiet; acknowledged, model-failed,
retry-ready, persistence-failed, and status-error boundaries each emit once per
exact message reconciliation. The local subscriber allowlist now includes both
Corpus lifecycle and delivery targets.

This instrumentation exposed a hardening gap: acknowledgement and retry-state
persistence returned booleans that the reconciler ignored. It now verifies
every grouped child mutation. An acknowledgement whose durable identity drifted
returns a visible error, emits `persistence_failed`, and remains retryable
instead of claiming success. Hermetic tests lock grouped acknowledgement and
message identity, non-retryable model failure, model-switch retry readiness,
and the adversarial persistence race. Strict Clippy, full downstream tests
including the hermetic curator campaign, both app feature configurations,
dependency policy, formatting, and diff checks pass. Real Qwen3.8 MLX probes
remained ignored and no model was started. The next dependency slice evaluates
Syntect's pure-Rust regex backend against the bundled syntax behavior and build
graph before changing the backend.

The sixth slice completed that dependency campaign and switched Corpus's
Syntect 5.3.0 configuration from `regex-onig` to the supported pure-Rust
`regex-fancy` backend. `default-syntaxes` already implies `parsing`, so the
redundant explicit feature was removed as well. The resolved Corpus graph now
contains `fancy-regex 0.16.2` under Syntect and contains neither `onig` nor
`onig_sys`, eliminating this feature's native C compilation and link edge.
Goose's independently constrained `fancy-regex` 0.17 and 0.19 packages cannot
be unified with Syntect's 0.16 requirement; their source and features remain
untouched.

The compatibility fixture now exercises representative editable Markdown
(headings, emphasis, links, fenced JSON, and inline code) plus an agent JSON
configuration containing strings, numbers, booleans, and punctuation. Both
backends preserve source and the Corpus palette contract. In an unoptimized
test process the cold focused suite takes about 0.04 seconds with Oniguruma and
0.69 seconds with the pure-Rust backend because regexes compile lazily. After
that initialization, a temporary probe measured 100 distinct uncached edits at
about 124 milliseconds total (roughly 1.24 milliseconds each). The probe was
removed after measurement. Corpus confines this parser to small editable
fields and caches exact rendered documents, so steady-state latency and the
portability/supply-chain tradeoff are acceptable. Strict Clippy, full
downstream tests including the hermetic curator campaign, both app feature
configurations, dependency policy, formatting, and diff checks pass. Real
Qwen3.8 MLX probes remained ignored and no model was started. The next
dependency slice designs the persisted-YAML compatibility campaign before any
replacement is selected.

The seventh slice established that campaign without changing the parser.
[`yaml-compatibility.md`](yaml-compatibility.md) now inventories every durable
and shipped surface, its trust/failure behavior, the locked fixture matrix,
the candidate trial order, and the replacement acceptance gate. All production
YAML parsing and serialization now enters through `corpus_store::yaml`, which
owns backend-neutral `from_str`, `from_value`, `to_string`, `to_value`, mapping
and value types, plus stable one-based error locations. Store errors no longer
expose `serde_yaml::Error`, and `corpus-observe` consumes the adapter for the
shipped model registry.

The migration removed direct `serde_yaml` edges from `corpus-observe` and the
`corpus-core` test target, plus a dead `corpus-integration` declaration: the
scenario YAML is copied into evidence but never parsed. Corpus now has one
direct deprecated-backend owner in `corpus-store`; Goose's independent
transitive use remains untouched. New adapter tests lock ambiguous strings,
Unicode, booleans, integers, nested ordered maps, current unknown-field rewrite
semantics, and actionable malformed locations. Model-registry tests add typed
scalars, unknown fields, and malformed-location behavior through its production
loader. Existing exact role/frontmatter, project, preference, agent, mission,
finding, and curator campaigns form the rest of the gate.

Primary-source research puts the YAML organization's `yaml_serde 0.10` fork
first for a minimal compatibility trial, `serde-saphyr` second for a larger
pure-Rust/budgeted hardening trial, and Saphyr's announced but unreleased Serde
integration outside the selectable set. No replacement has been selected yet.
Strict Clippy, full downstream tests including the hermetic curator campaign,
both app feature configurations, dependency policy, formatting, and diff
checks pass. Real Qwen3.8 MLX probes remained ignored and no model was started.
The next dependency slice trials `yaml_serde` behind the adapter and compares
the complete serialized fixture set before deciding whether to retain it.

The eighth slice completed that trial and retained `yaml_serde 0.10.7`. Before
the swap, the archived backend's representative output was captured exactly,
including ambiguous-string quoting, order, nesting, booleans, integers, and
Unicode; byte-exact rendered-role fixtures and the finding writer also passed.
The same adapter, exact output, error locations, unknown-field behavior,
persisted store campaigns, and shipped-registry campaigns passed unchanged
after switching the workspace dependency alias to the YAML organization's
maintained fork.

The resolved graph now places `yaml_serde` and `libyaml-rs 0.3.0` solely below
`corpus-store`. Deprecated `serde_yaml 0.9.34+deprecated` remains reachable only
through untouched Goose. `libyaml-rs` documents itself as libyaml translated
from C to unsafe Rust with C2Rust, and `yaml_serde` has narrow unsafe internals;
this slice therefore claims maintained custody and identical data behavior,
not elimination of unsafe code. `serde-saphyr` remains the future pure-Rust,
budget-aware option if its larger semantic migration becomes justified.
Strict Clippy, full downstream tests including the hermetic curator campaign,
both app feature configurations, dependency policy, formatting, and diff
checks pass. Real Qwen3.8 MLX probes remained ignored and no model was started.
The next Phase 6 slice adds enforceable supply-chain policy for advisories,
licenses, sources, and explicitly reviewed exceptions.

The ninth slice added that enforcement and completed Phase 6. The new
[`deny.toml`](../deny.toml) evaluates the all-feature Linux and Apple Silicon
graphs with pinned `cargo-deny 0.20.2`: RustSec advisories and yanked packages
are gated, licenses are allowlisted, unknown registries and Git repositories
are denied, every Git source requires a full revision, and duplicate versions
fail unless an exact version carries a reviewed reason. Goose remains
untouched and is the sole allowed Git repository at its existing full
revision. The redundant wildcard lint is disabled because cargo-deny 0.20.2
cannot resolve that optional workspace-inherited Git dependency while running
the lint; source and revision policy still enforce the relevant boundary.

The first advisory pass found `RUSTSEC-2026-0258` and upgraded compatible `h2`
0.4.15 to 0.4.16. `RUSTSEC-2026-0194` and `RUSTSEC-2026-0195` remain narrowly
reviewed exceptions for `quick-xml 0.30.0` on Egui's Linux accessibility
`zbus_xml 4` path, whose requirement cannot accept the fixed 0.41 line. Their
local availability risk, 2026-11-26 review deadline, and upstream removal
trigger are recorded in
[`supply-chain-policy.md`](supply-chain-policy.md). The local wrapper verifies
the exact cargo-deny version, and CI pins the matching official action to an
immutable commit. Strict Clippy, the full downstream suite including the
hermetic curator campaign, both app feature configurations, package and
supply-chain policy, formatting, and diff checks pass. Real Qwen3.8 MLX tests
remained ignored and no model was started.

Exit: the codebase is smaller at the responsibility level, failures are
diagnosable, and every retained dependency has an explicit job.

### Phase 7: documentation closeout

- Document the final architecture and dependency graph.
- Record trust boundaries and security invariants.
- Split operator, developer, plugin, testing, and troubleshooting guidance.
- Collapse completed temporary plans into tracked decisions and remove stale
  implementation-history comments.

The first Phase 7 slice added the authoritative shipped-system map in
[`architecture.md`](architecture.md). It records executable composition,
normal workspace dependency direction, major module ownership, writable and
read-only roots, project-only run namespaces, launch/plugin separation,
curator completion flow, and the documentation ownership map. The repository
README now links tracked architecture, testing, plugin, and roadmap documents
instead of sending readers to machine-local `dev/` scratch.

The existing dependency-policy gate now checks the exact normal workspace
edges for every crate before its deeper admin-artifact tree check. This makes
the documented layering executable: a new crate, missing edge, or added edge
fails until the architecture and policy are deliberately reviewed together.
The graph check, existing admin boundary, formatting, and diff validation pass.
This documentation/policy slice did not change runtime behavior; real Qwen3.8
MLX tests remained ignored and no model was started. The next Phase 7 slice
promotes the Phase 1 threat-model baseline into the final security-invariants
reference and reconciles it with the shipped architecture.

The second slice completed that promotion. [`threat-model.md`](threat-model.md)
now states the shipped trust assumptions and non-guarantees, protected assets,
authority zones, and stable `SEC-*` invariants for identity, authorization,
filesystem publication, durable lifecycle state, process/protocol bounds,
audit custody, and serial model-test isolation. Each invariant names its
enforcing code owner and concrete hermetic evidence, while accepted dependency
risk remains in the dated supply-chain policy rather than being duplicated.
README and the contributor trust-domain summary now route readers to this
authoritative contract.

The referenced store, core, admin, research MCP, headless app-state, and
hermetic curator suites pass with 416 tests passed and four classified live or
platform tests ignored. Formatting and diff validation pass. Real Qwen3.8 MLX
tests remained ignored and no model was started. The next Phase 7 slice
extracts operator and troubleshooting guidance from the contributor monolith;
the existing testing and plugin guides remain their authoritative owners.

The third slice completed that audience split. [`operator-guide.md`](operator-guide.md)
now owns installation, data roots, project and mission setup, curator
orchestration, management chat, confirmation, lifecycle recovery, audit, and
upgrade workflows. [`troubleshooting.md`](troubleshooting.md) starts from
observable symptoms and routes refusals, launch/attach failures, durable
cleanup, plugin health, chat scope, model identity, dependency failures, and
escalation evidence to their owning subsystem. The architecture documentation
map and README link both guides.

`AGENTS.md` is now a 183-line contributor contract rather than a 597-line
operator/developer monolith. It retains workspace ownership, executable
dependency constraints, namespace/security invariants, hermetic and serial-MLX
rules, store/agent contracts, Goose custody, and change discipline. The
existing testing and plugin documents remain authoritative and were not
duplicated. Documentation links, CLI contract tests, dependency policy,
formatting, and diff checks pass. This slice changed no runtime behavior, kept
live Qwen3.8 MLX tests ignored, and started no model. The next Phase 7 slice
collapses completed temporary plans into tracked decisions and removes stale
implementation-history comments without erasing useful rationale.

The fourth slice completed that decision hygiene. [`decisions.md`](decisions.md)
is now the tracked, fresh-checkout record of durable rationale for identity-
bound lifecycle, paint-thread isolation, deterministic runtime seams, bounded
OpenCode HTTP use, admin/research separation, project-local Super authority,
external immutable plugins and Goose custody, embedded-chat scoping,
exact-origin Curator orchestration, persisted-data adapters, and dependency
custody. It links to the current architecture, threat model, tests, and policy
instead of preserving obsolete branches, chunk numbers, line numbers, model
examples, or plugin release locks.

The ignored `dev/` directory is again temporary working memory. Eight completed
or superseded plan/evidence artifacts were removed after their durable outcomes
were captured: the frame-loop, chat-harness, chat-tool UX, curator
orchestration, documentation-hygiene, and OpenCode spike records plus the old
local decision ledger and spike script. `dev/ROADMAP.md` and `dev/TODO.md` now
name only active retheme/finding work, plans that require revalidation before
implementation, first-run setup, and an independent security review. Embargoed
research and current UI mocks remain local and untouched.

Shipped Rust and fallback-recipe comments no longer cite missing `dev` plans or
describe completed phase/chunk work as future behavior. Strict headless Clippy
passes. Focused app, CLI, core, observe, and integration suites pass 276 tests
with five classified platform/live tests ignored, including the hermetic
Curator campaign. Documentation links, architecture policy, formatting, and
diff checks pass. No live Qwen3.8 MLX test ran and no model started. The final
Phase 7 slice is a repository-wide closeout gate and merge-readiness audit.

The final slice completed that audit. The locked ten-package workspace builds;
all default-feature targets pass with 474 tests passed and eight deliberately
classified live, Docker/plugin, discovery, or platform tests ignored; strict
workspace Clippy, formatting, diff validation, dependency architecture, tracked
documentation links, and the `cargo-deny 0.20.2` supply-chain policy all pass.
The supply-chain run requires `GIT_CONFIG_GLOBAL=/dev/null` on this workstation
because its global Git configuration rewrites HTTPS GitHub URLs to SSH; this is
a host configuration issue, not a repository exception. CI action references
are pinned to full reviewed commits, and the model workflow now invokes the
library-hosted embedded probes plus the complete Curator system campaign
sequentially.

The required live `qwen3.8:27b-mlx` gate also passes under the global model
lease: one configured-model smoke probe, three embedded management-chat probes,
and the full two-child Curator orchestration campaign. The audit first caught
two false assumptions rather than hiding them: the split application had moved
embedded probes from the binary to the library target, and a run inside an
existing tmux server did not inherit the test-only plugin-catalog override.
The workflow/documentation now select `--lib`; run-local OpenCode MCP
configuration explicitly carries a non-empty `CORPUS_PLUGINS_DIR`; and the live
campaign asserts that propagation before spending a model turn. The preserved
first-attempt artifact shows the exact child session remained active in a tool
loop after `corpus_target_info` could not resolve `noop-integration`. After the
fix, both children launched one at a time, both exact completions were delivered
to the Curator, coordinator restart recovery succeeded, and the campaign passed
in 169.76 seconds. Goose remains unchanged at its existing full revision.

Exit: tracked documentation describes the shipped system and its verification
commands accurately.

## 12. Change discipline

- Separate mechanical movement from behavioral changes.
- Keep commits small enough to review one invariant at a time.
- Add characterization tests before changing unclear behavior.
- Run Tier A after every slice and Tier B after changes to chat, prompts, tools,
  MCP, state, store, launch, permissions, or transcripts.
- Do not relax a security assertion merely to make an extraction easier.
- Do not use file-size reduction as proof of architectural improvement.
- Do not create a new crate unless its boundary is enforceable and documented.
- Preserve failure artifacts for flaky or model-dependent behavior.

## 13. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Model tests are slow or nondeterministic | Global serial execution, pinned digest/settings, contract assertions, first-attempt reliability tracking |
| Large moves hide behavior changes | Mechanical commits, characterization tests, Tier A/Tier B gates |
| Trust boundaries weaken during API cleanup | Dependency policy, scoped integration tests, declarative authorization metadata |
| New helper crates increase supply-chain exposure | Add only when they delete substantial custom code or improve security; gate with cargo-deny |
| YAML replacement changes persisted documents | Central adapter plus complete fixture and round-trip corpus |
| Platform tests fail for environmental reasons | Explicit preflight, separate prepared runners, classified failure artifacts |
| Goose crate migration changes behavior | Treat as deliberate upgrade; run complete serial model suite and retain adapter boundary |
| Refactor collides with feature work | Land or isolate in-flight changes; serialize work through interface commits |

## 14. First implementation milestone

The first milestone should deliver, in order:

1. a selected clean integration baseline;
2. green formatting, lint, build, and hermetic-test CI;
3. the dedicated integration harness;
4. globally serial Qwen3.8 MLX execution;
5. migrated management-chat live probes with durable artifacts;
6. authorization, mission-launch, and recovery scenarios;
7. the threat model and first adversarial filesystem/process tests;
8. the initial `AppState` extraction behind those gates.

No broad modularization should begin before items 1-7 are operating. They are
the mechanism that makes the remainder of the refactor safe.
