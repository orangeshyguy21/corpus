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
- add real integration coverage with the local Qwen3.8 model;
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

### 2.2 Qwen3.8 inference is globally serial

The local machine can run only one model workload at a time. Every integration
test that uses Qwen3.8 must execute globally serially.

`RUST_TEST_THREADS=1` is not sufficient because separate Cargo processes or
test binaries can still overlap. The integration harness must take an exclusive
cross-process file lock before loading or calling the model. The lock owns the
entire model-backed suite, records its owner, has a bounded acquisition timeout,
and is released on normal process exit.

Within the lock:

- use one shared Ollama instance;
- load one pinned Qwen3.8 model and digest;
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
   Qwen3.8 model.
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

#### Tier B: required Qwen3.8 smoke suite

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

The three existing ignored live probes in `chat/embedded.rs` should be moved
into this harness once equivalent coverage is demonstrably running. Their
behavior stays; only test ownership changes.

#### Tier C: nightly full-system suite

Run globally serially with Qwen3.8 and actual platform services:

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
  scenarios.
- Implement global Qwen3.8 locking and artifact capture.
- Write the trust-boundary threat model.
- Add high-priority traversal, symlink, stale-target, malformed-reply, timeout,
  and cleanup tests.

Exit: the main user journeys are protected before structural extraction.

### Phase 2: application state decomposition

- Extract pure models and projections.
- Extract plugin and corpus coordinators.
- Extract run model, coordinator, launch, teardown, and recovery.
- Extract dispatch request, delivery, and completion handling.
- Reduce `AppState` to composition, navigation-facing projections, and intent
  routing.

Exit: no workflow depends on incidental mutation ordering inside one giant
implementation block; Tier A and Tier B remain green.

### Phase 3: store decomposition and filesystem hardening

- Introduce typed mutation requests.
- Centralize atomic and exclusive filesystem operations.
- Split project, mission, corpus entry, cost, run record, and agent modules.
- Quarantine YAML representation and build compatibility fixtures.
- Pilot capability-relative filesystem handles.

Exit: callers express intent without constructing sensitive paths or partial
write sequences themselves.

### Phase 4: launch decomposition

- Introduce immutable `LaunchPlan`.
- Split command, process, tmux, transcript, session, and cleanup modules.
- Tighten environment construction and executable resolution.
- Preserve cancellation, transactional adoption, stale-result rejection, and
  retryable cleanup.

Exit: runner behavior is independently testable and ready for future backend
implementations.

### Phase 5: typed tools and CLI

- Define typed tool arguments and generated schemas.
- Build declarative registries with authorization and refresh metadata.
- Split admin and research handler modules.
- Move CLI parsing and help generation to Clap.
- Add binary-level integration coverage.

Exit: catalogs, dispatch, authorization, approval, audit, and invalidation
cannot drift silently.

### Phase 6: views, observability, and dependency cleanup

- Split large views after their state APIs settle.
- Land structured tracing across critical workflows.
- Align duplicate dependency lines where compatibility permits.
- Remove the direct image decoder dependency.
- Evaluate Syntect backend and YAML replacement.
- Add supply-chain policy.

Exit: the codebase is smaller at the responsibility level, failures are
diagnosable, and every retained dependency has an explicit job.

### Phase 7: documentation closeout

- Document the final architecture and dependency graph.
- Record trust boundaries and security invariants.
- Split operator, developer, plugin, testing, and troubleshooting guidance.
- Collapse completed temporary plans into tracked decisions and remove stale
  implementation-history comments.

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
4. globally serial Qwen3.8 execution;
5. migrated management-chat live probes with durable artifacts;
6. authorization, mission-launch, and recovery scenarios;
7. the threat model and first adversarial filesystem/process tests;
8. the initial `AppState` extraction behind those gates.

No broad modularization should begin before items 1-7 are operating. They are
the mechanism that makes the remainder of the refactor safe.
