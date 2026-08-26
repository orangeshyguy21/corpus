# Refactor baseline

This document records the reproducible starting point for the senior-developer
refactor. Update it when a phase changes a measured boundary; do not rewrite the
original measurements.

## Selected integration point

- Branch: `refactor`
- Base commit: `4726729`
- Recorded: 2026-08-25
- Rust toolchain: `1.97.1` from `rust-toolchain.toml`
- Goose policy: keep the current pinned embedded dependency unchanged until its
  upstream crate is available; treat any future migration as a deliberate
  compatibility change requiring the full integration suite.

## Phase 0 quality baseline

The refactor branch establishes these mandatory checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --workspace --no-default-features
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
./scripts/check-dependency-policy
```

Formatting and both strict Clippy configurations are green as of this baseline.
The workspace test command is the hermetic gate; tmux and live-service tests are
separately classified in `docs/testing.md`.

Build time, test time, binary size, source size, and dependency counts are
captured below after the clean Phase 0 gate completes. Local times are
diagnostic rather than acceptance budgets because they depend on cache and
hardware.

| Measurement | Value | Notes |
|---|---:|---|
| Rust source files | 71 | `crates/**/*.rs`; vendored code excluded |
| Rust source lines | 51,375 | physical lines in the 71 workspace files |
| Direct workspace dependencies | 18 | root workspace dependency declarations |
| Locked packages | 843 | all host and target-specific entries in `Cargo.lock` |
| Debug `corpus-app` size | 293,498,248 bytes | 279.9 MiB local macOS binary |
| Headless build time | 6.30 seconds | warm local target cache |
| Default build time | 7.96 seconds | warm local target cache |
| Hermetic test time | 51.82 seconds | warm local target cache; all tests passed |

The largest refactor targets at the integration point are `state.rs` (9,426
lines), `agents.rs` (3,924), `launch.rs` (2,578), `store.rs` (2,522), the admin
library (2,140), and the research MCP tools module (1,835). These are sequencing
inputs, not file-size-only acceptance criteria: behavior moves only after its
characterization and integration contracts exist.

The primary workflow is `.github/workflows/ci.yml`. Hosted CI owns format,
feature-off and default lint, feature-off and default builds, hermetic tests,
the existing dependency-boundary policy, and a prepared tmux platform job.
Qwen3.8 MLX jobs are added only with the Phase 1 cross-process lock and prepared
single-model runner.

## Phase 1 integration checkpoint — 2026-08-25

- Added the `corpus-integration` workspace package with isolated stores,
  bounded child processes, failure-bundle preservation, Ollama digest
  preflight, and an OS-backed cross-process model lease.
- Added an assembled hermetic curator campaign covering exact-origin child
  launches, two concurrent child records, artifact and no-artifact completion,
  grouped admission, wrong-parent rejection, retry, restart recovery, duplicate
  suppression, launch failure, and follow-up dispatch.
- Centralized launch-request consumption and dispatch delivery-state mutations
  behind checked store methods. Stale callers must match parent, child run,
  completion, and message identity before state advances.
- Completion records now carry explicit artifact paths, and curator completion
  prompts render those paths. Corpus does not infer ownership from project-wide
  file changes because overlapping children could claim each other's output.
- Verified real `qwen3.8:27b-mlx` inference at digest `5642e97495e1` and passed
  the embedded operator project, orchestrator delegation, and four-call depbot
  regression scenarios under the global lease.
- Added a manual self-hosted model workflow and the initial trust-boundary
  threat model.
- Exposed the application coordinator as a library with a synchronous headless
  system-test seam; the GUI binary and live campaign now use the same launch,
  session ownership, completion, and delivery implementation.
- Passed the final production curator campaign in 228.54 seconds using real
  OpenCode/tmux children. The gate proved terminal-turn launch serialization,
  two exact-origin completion acknowledgements, and coordinator restart
  recovery while the curator conversation remained live.

The Phase 1 integration gate is implemented. Before merge, rerun the full
workspace quality gates and the ignored model campaign on the prepared MLX
runner; preserve the first failure bundle if either fails.

## Phase 2 decomposition checkpoint — 2026-08-25

- Moved render-safe project, plugin, discovery, notice, mission-display, and
  run-lifecycle value types from `state.rs` into `state/models.rs`.
- Moved corpus revision guards, finding projection transitions, refresh job
  scheduling, stale-result retry, and corpus read projections into
  `state/corpus.rs`.
- Moved plugin discovery and stale-probe handling, lifecycle jobs and
  cancellation, durable lease drift projection, orphan cleanup, and operator
  recovery hints into `state/plugin.rs`.
- Moved run backend and environment-runtime seams, launchability guards, phase
  invariants, run identity, process adoption/rejection, exit polling,
  synchronous stop, and PTY projections into `state/run.rs`.
- Moved asynchronous preparation, fresh/detached/resume launch, cancellation,
  durable mission binding and adoption recovery, conversation capture,
  settled-turn export, and background teardown into
  `state/run/coordinator.rs`.
- Moved external session discovery, liveness and activity polling, scoped
  maintenance scheduling, restart-safe conversation recovery, repaint
  projection, and the pure activity/checkpoint predicates into
  `state/session.rs`.
- Moved durable curator/MCP launch and deletion request consumption, exact
  dispatch-origin preservation, activity/completion reconciliation, grouped
  parent delivery, acknowledgement, retry state, and the headless system-test
  facade into `state/dispatch.rs`.
- Moved project/agent/mission refresh, selection and CRUD, source revision and
  pin handling, environment projection, model discovery, scoped external
  refresh, and label/sort policy into `state/resources.rs`.
- Moved file-invalidation routing, job-runtime installation, scoped result
  application, stale-result retries, cancellation/timeout/failure mapping,
  source-revision application, and notice-resolution policy into
  `state/background.rs`.
- `AppState` remains the owner of process, session, store, and background-job
  handles; these checkpoints change module ownership, not workflow behavior.
- `state.rs` is now a 589-line composition root, down from 9,531 lines at the
  start of Phase 2. The nine production modules remain behind the unchanged
  `AppState` API.
- Partitioned the former 3,164-line inline regression module into shared
  fixtures in `state/tests.rs` and seven focused suites covering resources,
  sessions, dispatch requests, corpus projection, run lifecycle, checkpoint
  maintenance, and completion delivery. The largest state test file is now
  893 lines.
- Strict Clippy and application tests remained green with both default and
  feature-off configurations (152 feature-off tests; 158 default tests passed
  with three explicitly ignored live-model tests). The hermetic full curator
  campaign and dependency-policy gate also remained green.
- Phase 2 is complete: `AppState` is a composition root and ownership
  container, and its regression coverage is organized by workflow rather than
  retained in a second monolith.

## Phase 3 store-hardening checkpoint — 2026-08-25

- Replaced the positional `Store::create_agent` and `Store::add_subagent`
  mutation APIs with `CreateAgentRequest` and `AddSubagentRequest`.
- Propagated the typed requests through the admin handlers, app state and UI,
  store tests, and the serial Qwen3.8 MLX system-test fixture. Callers now name
  every mutation field instead of relying on adjacent string argument order.
- The request types are owned, immutable inputs at the store boundary; the
  existing document validation, role inference, delegation wiring, and stored
  representation remain unchanged.
- Added one `filesystem::atomic_write` primitive for same-directory file
  replacement. It allocates staging files with `create_new`, uses unique
  process-and-sequence names, syncs complete bytes before rename, and removes
  staging files on failure.
- Migrated project, mission, agent, environment-session, usage, preference,
  generated run/agent, and curated-entry overwrites to that primitive. The
  append-only audit/refusal logs, exclusive finding creation, and semantic
  corpus moves retain their distinct filesystem operations.
- Characterization tests cover successful replacement, preservation and
  cleanup on a failed rename, and concurrent writers publishing only one
  complete payload. No filesystem helper dependency was added.
- Extracted the `Project` record and its create, list, delete-request, guarded
  delete, clone, rename, plugin-rebind, pin, and corpus-wipe operations into
  `corpus-store/src/projects.rs`. Project-specific tree copy and category
  initialization moved with their owner.
- `store.rs` retains a compatibility re-export, so existing root and
  `store::Project` paths and every `Store` method signature remain unchanged.
  The file is now 2,413 lines, down from 2,668 before resource extraction; the
  focused project module is 235 lines.
- Extracted mission persistence into `corpus-store/src/missions.rs`: the
  mission/control/request/dispatch/completion records, legacy launch-request
  decoder, Markdown-frontmatter storage, CRUD, deletion guards, and exact-child
  completion delivery transitions now share one 413-line owner.
- `store.rs` retains compatibility re-exports for every mission type and all
  `Store` method signatures remain unchanged. It is now 1,944 lines. The
  hermetic curator campaign continues to cover origin binding, completion,
  admission, acknowledgement, retry, and restart-safe durable state across the
  extracted boundary.
- Extracted `EntryAccess`, relative-path validation, canonical containment,
  destination ancestor resolution, byte-counted delete, same-corpus move, and
  atomic entry write into the 220-line
  `corpus-store/src/corpus_entries.rs` security boundary.
- The module continues to refuse traversal, absolute paths, unknown or bare
  categories, mutation of run transcripts, and symlink escapes. Findings now
  import the policy from its owning module; `store::EntryAccess` remains a
  compatibility re-export. `store.rs` is now 1,703 lines.
- The dedicated curation and finding-contract suites remained green across
  traversal, symlink, immutable-transcript, recursive-delete, overwrite,
  project-scope, and collision-safe finding scenarios.
- Extracted compact usage snapshot models, snapshot persistence and legacy
  backfill, metadata-keyed cost caching, transcript fallback parsing,
  tool-adjusted inference timing, and per-model aggregation into the 406-line
  `corpus-store/src/accounting.rs` module.
- Root and `store::*` compatibility re-exports preserve the existing cost
  types and functions, while usage snapshot methods remain on `Store` through
  the extracted implementation. `store.rs` is now 1,287 lines, down from
  1,703 before this slice.
- Store accounting tests and application usage-checkpoint coverage remained
  green, including transcript replacement without double-counting, parallel
  tool-time adjustment, and cost independence from large exported transcripts.
  The hermetic full curator campaign also remained green; no local model was
  started for this structural slice.
- Extracted run identity constants, immutable transcript record projection,
  direct-file discovery, timestamp parsing, and mission-session-to-agent
  attribution into the 106-line `corpus-store/src/run_records.rs` module.
- Root and `store::*` compatibility re-exports preserve `MissionLog`,
  `mission_logs`, and all run identity/category constants. Runtime run-directory
  provisioning remains in `Store`; it constructs the launch workspace rather
  than reading persisted transcript records. `store.rs` is now 1,197 lines.
- Run-log characterization remains green for category separation, newest-first
  ordering, legacy names, session-keyed exports, exact run identity, and the
  immutable-transcript curation boundary. The full hermetic curator campaign
  also passed without starting a local model.
- Began decomposing the 3,992-line agent module by extracting sidecar metadata,
  loaded configuration, typed create/subagent mutation requests, role-migration
  results, and resolved source pins into
  `corpus-store/src/agents/model.rs` (103 lines).
- `agents.rs` publicly re-exports every moved contract, preserving all
  `corpus_store::agents::*` and crate-root paths. Serialization defaults retain
  the distinction between legacy missing role/provenance and explicit safe
  values. `agents.rs` is now 3,876 lines.
- Agent persistence, clone, migration, role-ceiling, rendering, downstream API,
  and hermetic curator tests all remained green. No local model was started.
- Extracted `AgentRole`, operator-facing role metadata and prompts, sandbox and
  project-management tool catalogs, capability checks, web/shell decisions,
  explicit subagent authority ceilings, and legacy permission inference into
  `corpus-store/src/agents/roles.rs` (289 lines).
- `agents.rs` re-exports the complete public role surface. Renderer-only catalog
  completeness and migration ordering remain visible only within the parent
  agent module, preventing them from becoming unsupported public API.
  `agents.rs` is now 3,453 lines.
- Role catalog totality, parse round-trips, legacy inference, researcher/tester/
  curator/super containment, MCP authorization fixtures, rendered-role
  fixtures, application policy projections, and the hermetic curator campaign
  all remained green. No local model was started.
- Extracted render context, normalized permission actions and rule families,
  role/tool ceilings, stored-policy tightening, concrete-project rebinding,
  declared-task sealing, absolute store/data-root denial, immutable transcript
  protection, canonical JSON ordering, and safe YAML scalar emission into
  `corpus-store/src/agents/permissions.rs` (478 lines).
- The module exposes only a parent-module rendering seam; permission internals
  remain private and no new crate-level API was added. `agents.rs` is now 2,986
  lines, down from 3,453 before this slice.
- Characterization and downstream role fixtures remained green for missing and
  scalar permission families, stored deny/ask tightening, cross-project path
  denial, role-derived tools, shell/external denial, task closure, deterministic
  output, and immutable run logs. The hermetic curator campaign also passed
  without starting a local model.
- Extracted project-wide and additive agent rendering, generated-directory
  cleanup, primary handle derivation and collision disambiguation, flat rendered
  name claims, delegation-closure validation, entry-role selection, frontmatter
  assembly, corpus/source orientation footers, and prompt-file expansion into
  `corpus-store/src/agents/rendering.rs` (475 lines).
- All `Store` rendering methods and the public `primary_handles` function retain
  their existing paths. Renderer helpers are internal, and the permission
  module now imports its private tool catalog directly rather than through the
  facade. `agents.rs` is now 2,531 lines.
- Handle, collision, dangling/cross-agent delegation, deterministic role
  fixture, prompt materialization, project-wide regeneration, additive render,
  launch materialization, and hermetic curator coverage remained green. No
  local model was started.
- Extracted primary-agent selection, OpenCode document and agent-map structure,
  exactly-one-primary enforcement, recursive permission-shape checks, and
  prompt-reference validation into `corpus-store/src/agents/validation.rs`
  (151 lines). `agents.rs` is now 2,442 lines.
- Closed a prompt-reference containment flaw discovered during extraction. The
  old existence check accepted an existing `../` target or a symlink inside an
  agent directory that resolved outside it. The shared resolver now rejects
  absolute and non-normal components, canonicalizes both roots and targets,
  requires a regular file beneath the canonical agent directory, and gives the
  renderer that canonical path so hand edits or symlink changes cannot bypass
  the save-time check.
- Added traversal and Unix symlink regression coverage. Store, downstream,
  both application feature configurations, and hermetic curator tests remained
  green; the real Qwen3.8 MLX tests remained ignored and no model was started.
- Extracted atomic OpenCode JSON writes, sidecar creation and provenance
  stamping, fail-closed sidecar reads, and recursive agent-tree copying into
  `corpus-store/src/agents/repository.rs` (159 lines). All persistence helpers
  remain private to the agent module; `agents.rs` is now 2,371 lines.
- Hardened create-from, clone, and cross-project copy against source and
  destination symlinks and other special files. The previous recursive copier
  followed links, allowing content outside the source agent tree to be imported
  and allowing a planted destination link to redirect copied bytes. The new
  copier preflights the complete source, rechecks entries while copying, and
  atomically claims a new destination directory before writing into it.
- Added a Unix regression proving source and destination symlinks are refused,
  external content is unchanged, and a source-preflight refusal publishes no
  partial destination. All downstream and feature-policy gates remained green;
  no local model was started.
- Moved the first public `Store` lifecycle boundary into `agents/repository.rs`:
  sorted list, fail-closed load, validated atomic save, config hashing, sidecar
  name and role mutations, durable delete requests, mission ownership lookup,
  and the guarded agent-delete cascade. Existing method paths and results are
  unchanged. The repository module is now 295 lines and `agents.rs` is 2,223.
- Agent deletion still preflights every assigned mission before removing any
  record, so moving ownership did not weaken the all-or-nothing cascade guard.
  Store, downstream, both app feature configurations, hermetic curator, strict
  Clippy, formatting, diff, and dependency-policy gates remained green. The
  real Qwen3.8 MLX tests remained ignored and no model was started.
- Moved role-based creation, typed creation with inheritance, same-project
  clone, and cross-project copy into `agents/repository.rs`, consolidating both
  clone variants behind one primary-key rewrite and publication path. The
  repository module is now 544 lines and `agents.rs` is 2,004.
- Closed three lifecycle gaps exposed by consolidation: clone/copy previously
  treated a missing/invalid primary as success, failures after the tree copy
  left a destination agent behind, and cross-project copy allowed a pending
  source agent or pending destination project. Sources are now fully validated
  before copying, all creation paths use a cleanup guard until document and
  sidecar persistence succeeds, and pending-deletion ownership is enforced.
- Added regressions for invalid inherited prompts, malformed source documents,
  unpublished-directory cleanup, pending source agents, and pending destination
  projects. Store, downstream, both app feature configurations, hermetic
  curator, strict Clippy, formatting, diff, and dependency-policy gates stayed
  green; no local model was started.
- Moved field edits, permission patches, subagent add/remove and delegation,
  subagent role assignment, and legacy role migration into the private
  321-line `corpus-store/src/agents/mutations.rs`. Shared lookup, validation,
  delegation, and role-compatibility helpers now keep those mutations on one
  policy path; `agents.rs` is now 1,695 lines.
- Fixed a partial-add bug exposed by the extraction. An incompatible requested
  subagent role was previously rejected only after the agent document and
  delegation grant had been saved. Role compatibility is now preflighted, a
  regression proves the refused subagent is not published, and add/remove
  best-effort restore the original document if role-sidecar persistence fails.
- Partitioned all 29 agent characterization tests into focused role, mutation,
  repository, and rendered-policy suites under
  `corpus-store/src/agents/tests/`, with only shared fixtures in `tests/mod.rs`.
  The production `agents.rs` facade is now 58 lines; the largest focused test
  module is 553 lines. Test count, names, platform conditions, and assertions
  are preserved.
- Extracted source-cache, mutable-var, run, and chat path composition; scoped
  run-workspace provisioning; generated OpenCode configuration; and symlink
  replacement into the private 290-line `corpus-store/src/run_workspace.rs`.
  Public `Store` method paths remain unchanged, the three existing workspace
  characterizations moved with their owner, and `store.rs` is now 917 lines
  with production code ending at line 429.
- Closed a workspace containment gap exposed by the extraction. Provisioning
  previously followed planted symlinks in managed parent positions and did
  not reject an extra project entry left in the supposedly single-project
  namespace. Managed directory components must now be real directories and
  the project namespace must contain only the selected project. A Unix
  regression proves both attacks fail before publishing the selected link or
  writing through the planted parent.
- Extracted durable application preferences into the 76-line
  `corpus-store/src/preferences.rs` and corpus size/category/log projection
  into the 235-line `corpus-store/src/corpus_stats.rs`. Existing crate-root,
  `store::*`, and public `Store` method paths remain compatible. The two
  corpus-stat characterizations moved with their owner, and preference
  malformed-input/default plus round-trip coverage was added. `store.rs` is
  now 657 lines, with production code ending at line 267.
- Fixed corpus-summary root containment. Nested symlinks were already ignored,
  but replacing the corpus root itself with a symlink caused `Path::is_dir` to
  follow and summarize an external tree. Root metadata now fails closed unless
  it is a real directory; a Unix regression covers both root and nested links.
- Moved the final ten `store.rs` characterizations to their accounting,
  project, mission, and run-record owners. Test names, assertions, and fixture
  semantics remain unchanged; each production owner now declares its focused
  test module. `store.rs` is a 263-line production-only composition facade with
  no embedded characterization body.
- Phase 3 store decomposition is complete: resource persistence, agent policy,
  atomic filesystem mutation, run workspaces, preferences, projections, and
  their characterizations now have explicit owners while compatibility
  re-exports preserve downstream paths.
- Strict Clippy and affected crate tests passed, including both application
  feature configurations and the hermetic full curator campaign. Formatting,
  diff validation, and the dependency-policy gate also remained green.

## Phase 4 launch-decomposition checkpoint — 2026-08-25

- Added the private, immutable 224-line
  `corpus-core/src/launch/plan.rs`, owning project, agent, resolved/optional
  model, prompt, source pins, environment-session identity, exact mission/run
  identity, and mutually exclusive interactive, resume, and headless/append
  execution modes.
- All interactive, resume, headless, and append compatibility entry points now
  copy their positional inputs into one owned `LaunchPlan` before backend
  selection or command construction. TUI and piped constructors accept only
  `&LaunchPlan`; their former eight/nine-argument seams and Clippy exemptions
  are gone.
- Extracted plan-derived child-environment projection, typed backend identity,
  piped OpenCode command assembly, TUI script rendering, and shell quoting into
  the private 194-line `corpus-core/src/launch/command.rs`. Both launch
  backends now consume the same owned environment projection: the TUI script
  and tmux session receive identical values, while the piped backend applies
  that projection to an unspawned `Command`.
- The command seam introduces no dependency or long positional helper. Added
  focused characterizations for exact mission/run/environment identity and
  safe quoting of every dynamic shell value. The remaining `launch.rs` facade
  is 2,481 lines.
- Extracted OpenCode/tmux discovery, executable-bit checks, cached tmux
  fallback resolution, the `CORPUS_NO_TMUX` override, and tmux version
  capability parsing into the private 171-line
  `corpus-core/src/launch/executables.rs`. OpenCode retains its explicit PATH,
  user-installation, then resource-tree precedence; tmux retains PATH before
  Homebrew, MacPorts, and system locations.
- Added deterministic characterizations for all three OpenCode precedence
  levels and the supported tmux 3.2, suffix, `next-*`, and newer-major forms.
  Resolution policy is now testable without mutating process-global PATH or
  launching either binary. `launch.rs` is now 2,387 lines.
- Extracted detached-session creation, session environment projection, mouse
  and raw-capture setup, external/embedded attachment, throttled liveness, and
  race-aware checked teardown into the private 215-line
  `corpus-core/src/launch/tmux.rs`. Public attach and teardown paths remain
  compatible through facade re-exports, while `launch.rs` no longer assembles
  raw tmux commands.
- The extraction exposed an unquoted `pipe-pane` capture path. A store path
  containing spaces or an apostrophe could break the capture shell command;
  the adapter now quotes the complete path. Focused characterizations lock
  detached-session argument separation, embedded attachment, and capture-path
  quoting. `launch.rs` is now 2,260 lines.
- Extracted owned process-group setup, piped child spawning, typed stdout and
  stderr pumping, bounded command output, timeout termination, and checked
  tree reaping into the private 315-line
  `corpus-core/src/launch/process.rs`. The command module now only constructs
  unspawned commands; all process ownership begins at the adapter boundary.
- Piped launch now opens its durable transcript before spawning, closing a
  failure window where transcript setup could fail after leaving a child
  running. Focused characterizations cover output caps and stream separation,
  typed line/log pumping, and timeout cleanup of the full process group.
  `launch.rs` is now 2,041 lines.
- Extracted artifact/run-stem naming, piped transcript creation and headers,
  raw-tail buffering, OpenCode conversation discovery, bounded JSON export,
  valid-record fallback, and the public session transcript projections into
  the private 449-line `corpus-core/src/launch/transcript.rs`. Existing public
  export, raw-log, idle-age, and conversation paths remain compatible through
  facade re-exports.
- Conversation discovery previously used an unbounded `Command::output()` and
  could hang the caller indefinitely. It now uses owned process supervision
  with a 10-second deadline and 1 MiB output cap. Characterizations cover
  oldest eligible/unclaimed selection, transient truncated-export retry,
  capped-export fallback, buffered tail draining, and mission-distinct stems.
  `launch.rs` is now 1,587 lines.
- Extracted `RunSession` backend state, typed output, launch identity and
  control-port projection, throttled conversation discovery, output/exit
  observation, attach commands, stop results, transcript export, fallback,
  and checked cleanup into the private 267-line
  `corpus-core/src/launch/session.rs`. The public `RunLine`, `RunSession`, and
  `StopOutcome` paths remain compatible through facade re-exports.
- The session owner composes the process, tmux, and transcript boundaries
  instead of assembling subprocesses itself. Launch construction and legacy
  spawn/resume adapters remain in the facade for the next bounded extraction;
  `launch.rs` is now 1,256 lines, down from 1,587 before this slice.
- Extracted interactive, resume, headless, append, environment, and exact-run
  compatibility entry points; immutable-plan routing; and TUI/piped backend
  construction into the private 440-line
  `corpus-core/src/launch/start.rs`. It composes plan, command, executable,
  process, tmux, transcript, and session owners without widening any public
  API.
- The split exposed an implicit dependency from `command.rs` on a facade
  re-export of `LaunchPlan`; command construction now imports its plan owner
  directly. `launch.rs` is 839 lines, with production code ending at line 290
  and the remaining body consisting of launch characterization tests.
- Extracted primary-agent/explicit/registry model selection, loopback control
  port allocation, stable per-run 0600 control credentials, and public agent
  file/handle identity projection into the private 221-line
  `corpus-core/src/launch/policy.rs`. Public model, credential, and identity
  paths remain compatible through facade re-exports.
- `start.rs` now imports policy directly and owns its launch timestamps;
  `command.rs` also imports agent-handle policy from the owner instead of
  reaching through the facade. Model precedence, bare-name identity, and
  credential privacy characterizations moved with policy. `launch.rs` is now
  622 lines, with production code ending at line 119.
- Partitioned the remaining eleven facade characterizations into focused
  construction (379 lines), observation (69 lines), and compatibility (47
  lines) suites under `corpus-core/src/launch/tests/`, with only two shared
  environment/store fixtures in the 33-line support module. Test names,
  assertions, global environment locking, and the platform tmux ignore remain
  intact.
- `launch.rs` is now a 97-line production composition and re-export facade.
  Phase 4 is complete: immutable planning, environment/command construction,
  executable policy, process and tmux ownership, transcript custody, session
  lifecycle, startup routing, model/control/identity policy, and their focused
  characterizations have explicit owners without changing the public runner
  API.
- Added characterizations proving the plan owns caller data and that resume
  and append inputs cannot overlap. Existing model precedence, exact Curator
  return identity, source/environment propagation, transcript naming, TUI,
  piped, teardown, downstream, feature-off, and hermetic curator tests remain
  green. The real Qwen3.8 MLX tests stayed ignored and no model was started.

## Phase 5 typed-tools checkpoint — 2026-08-25

- Added `docs/tool-registry-inventory.md`, mapping the 35 host-admin, up to 29
  role-scoped management, 10 research/environment, and management-chat tool
  surfaces to their current catalog, dispatch, authorization, classification,
  confirmation, audit, and UI invalidation owners.
- Began typed migration with `model_list`. Its Serde `ModelListArgs` and
  generated `schemars` input schema now share one 66-line owner in
  `corpus-admin/src/tools/models.rs`; the catalog no longer duplicates its
  field types, defaults, or descriptions by hand.
- Empty/default and unknown-field compatibility remain intact. A supplied
  `filter` or `refresh` with the wrong type is now rejected as invalid
  arguments instead of silently behaving as if the field were absent.
- Added direct `serde` and `schemars` dependencies to `corpus-admin`.
  `schemars` 1.2.2 was already locked transitively, so this added no package to
  the dependency graph. Host/scoped catalog, admin/research MCP, downstream,
  feature-off, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Added the private 155-line `corpus-admin/src/tools/registry.rs` declarative
  boundary. Each migrated `ToolDefinition` owns its name, description,
  generated schema, handler, and a policy that classifies capability,
  read/write/destructive behavior, confirmation, audit category, and UI
  refresh area. Registry validation refuses policy combinations that omit the
  required confirmation, audit, or refresh behavior.
- Completed the first vertical migration. Both admin catalog generation and
  dispatch now resolve `model_list` through the same definition, and its full
  handler moved beside `ModelListArgs` in the 152-line
  `corpus-admin/src/tools/models.rs`. The handwritten catalog object, dispatch
  arm, and handler left the 2,100-line admin facade.
- Added characterizations for unique and policy-complete definitions,
  definition-to-catalog projection, generated-schema/deserializer agreement,
  compatibility defaults, strict present-field types, and dispatch through
  the registry before model discovery. Focused admin/MCP tests, the full
  downstream suite, both application feature configurations, the hermetic
  curator campaign, strict Clippy, formatting, diff, and dependency policy
  remain green. Real Qwen3.8 MLX tests stayed ignored and no model was started.
- Migrated all five project tools into the focused 348-line
  `corpus-admin/src/tools/projects.rs`. Typed inputs, generated schemas,
  descriptions, handlers, and policies for list, create, clone, delete, and
  rebind now share their domain owner; their handwritten catalog entries,
  dispatch arms, and handlers left the facade, reducing it from 2,100 to 1,924
  lines.
- Project policies explicitly distinguish the read-only listing; audited,
  refresh-producing writes; and confirmation-gated deletion. Delete continues
  to use the existing one-shot token and durable mission-teardown path, while
  rebind continues to validate the discovered plugin registry before writing.
  The mixed declarative/legacy catalog is sorted through canonical
  `ADMIN_TOOLS` order so incremental migration does not reorder its public
  surface.
- Typed optional project arguments preserve missing/default and unknown-field
  compatibility. Present wrong types—including a non-boolean corpus clone
  flag or non-string confirmation token—now fail before persistence or plugin
  discovery. Focused schema/policy, host/scoped catalog,
  destructive-confirmation, audit, curator, downstream, feature-off, hermetic
  curator, strict Clippy, formatting, diff, and dependency-policy gates remain
  green. Real Qwen3.8 MLX tests stayed ignored and no model was started.
- Moved typed `agent_list` and `agent_get` definitions and handlers into the
  focused 130-line `corpus-admin/src/tools/agents.rs`. Their handwritten
  catalog entries and routing arms left the admin facade, which is now 1,861
  lines. Both operations are explicitly read-only, unaudited, unconfirmed,
  and non-refreshing.
- Centralized generic generated-schema and typed-deserialization plumbing in
  the registry; model, project, and agent domain owners now provide only their
  contracts and behavior. Required wrong or missing agent fields fail before
  store access, while unknown future fields remain compatible.
- Added a direct host/scoped schema characterization: host `agent_list` and
  `agent_get` require project identity, scoped catalogs remove exactly that
  launcher-proven field, and scoped `agent_get` still requires its agent
  identity. Focused admin/MCP, downstream, both app feature configurations,
  hermetic curator, strict Clippy, formatting, diff, and dependency-policy
  gates remain green. Real Qwen3.8 MLX tests stayed ignored and no model was
  started.
- Migrated validated `agent_new`, `agent_save`, `agent_clone`, and `agent_copy`
  into the now 346-line `corpus-admin/src/tools/agents.rs`. Their typed
  contracts, generated schemas, declarative definitions, and handlers left the
  handwritten catalog and facade router; `corpus-admin/src/lib.rs` is now
  1,722 lines.
- The role choices advertised by `agent_new` are now the executable typed
  enum, and `agent_save` rejects non-object documents before invoking the core
  validator. Inheritance and destination defaults remain compatible, while
  create, save, same-project clone, and cross-project copy continue through
  their existing validated store boundaries. Each is explicitly classified as
  an unconfirmed, `agents`-audited, `agents`-refreshing write.
- Characterizations cover generated required fields and roles, invalid role
  and document shapes, future-field compatibility, copy defaults, and policy.
  Existing save round-trips, scoped authorization, mutation auditing, agent
  repository, downstream, both app feature configurations, hermetic curator,
  strict Clippy, formatting, diff, and dependency-policy gates remain green.
  Real Qwen3.8 MLX tests stayed ignored and no model was started.
- Migrated `agent_set`, `agent_set_role`, `agent_set_permission`,
  `agent_subagent_add`, and `agent_subagent_remove` into the now 638-line typed
  agent domain. Their schemas, definitions, routing, and handlers left the
  facade, reducing `corpus-admin/src/lib.rs` from 1,722 to 1,519 lines.
- Generated enums are now executable for scalar field and role choices, and
  permission patches are typed as objects. Arbitrary JSON field values still
  allow `null` clearing, and empty optional subagent names still select the
  primary entry. Existing store boundaries continue to enforce surgical
  validation, role ceilings, delegation consistency, compatibility preflight,
  and rollback. All five tools carry the shared unconfirmed, `agents`-audited,
  `agents`-refreshing write policy.
- Characterizations cover null clearing, empty-subagent compatibility, invalid
  fields, roles and patch shapes, and policy. Focused store mutation/role,
  scoped authorization/audit, downstream, both app feature configurations,
  hermetic curator, strict Clippy, formatting, diff, and dependency-policy
  gates remain green. Real Qwen3.8 MLX tests stayed ignored and no model was
  started.
- Moved typed, confirmation-gated `agent_delete` into the focused 210-line
  `corpus-admin/src/tools/agents/delete.rs`. Its schema, definition, dry-run
  consequence projection, one-shot execution, mission cascade reporting,
  durable teardown request, and dangling-delegation analysis now share one
  destructive owner; the admin facade is down from 1,519 to 1,385 lines.
- The delete definition requires token confirmation and carries `agents` audit
  and refresh metadata. A present non-string token now fails before preview or
  mutation. Existing scoped curator coverage continues to prove the assigned
  mission is named and deleted with its agent, unrelated missions survive, and
  live guards defer to durable app teardown.
- Added focused schema/policy, mission-suffix, and allowed-versus-denied
  delegation characterizations. Host/scoped catalog, confirmation, audit,
  agent repository, app lifecycle, downstream, both feature configurations,
  hermetic curator, strict Clippy, formatting, diff, and dependency-policy
  gates remain green. Real Qwen3.8 MLX tests stayed ignored and no model was
  started.
- Partitioned the behavior-complete 642-line typed agent domain into focused
  owners: a 126-line read module, 204-line persistence module, 273-line
  granular-mutation module, existing 210-line deletion module, and 35-line
  shared role/write-policy contract. `tools/agents.rs` is now a 12-line
  composition facade.
- Contract and policy tests moved beside their owning behavior. The registry,
  canonical catalog order, dispatch, schemas, compatibility defaults, store
  calls, output text, authorization, audit, confirmation, and refresh behavior
  are unchanged. Focused admin/MCP, downstream, both app feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Migrated immediate `mission_list`, `mission_get`, and `mission_status` reads
  into the focused 193-line `corpus-admin/src/tools/missions.rs`. Their typed
  arguments, generated schemas, definitions, policies, routing, and handlers
  now share one owner; the admin facade fell from 1,385 to 1,278 lines.
- All three definitions are explicit unconfirmed, unaudited, non-refreshing
  reads. Required identities, optional single-mission selection, unknown-field
  compatibility, and policy are characterized. A present non-string optional
  mission now fails before observation instead of silently selecting all.
  Existing curator status, host/scoped catalog, downstream, both app feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. The blocking operator-only
  `mission_await` stays separate for the next slice. Real Qwen3.8 MLX tests
  stayed ignored and no model was started.
- Isolated typed `mission_await` in the focused 285-line
  `corpus-admin/src/tools/missions/wait.rs`, moving its schema, definition,
  bounded polling handler, snapshots, corpus-output diff, and tests out of the
  facade. `corpus-admin/src/lib.rs` fell from 1,278 to 1,029 lines.
- Fixed a catalog defect exposed by the typed contract: `timeout_secs` was
  documented and accepted by dispatch but absent from the handwritten input
  schema. It is now generated from `MissionAwaitArgs`; non-unsigned inputs fail
  before blocking. Pure tests cover the 45-second default, 1/90-second clamps,
  exact-one/all selection, state transitions, unchanged state, and new output.
  Existing scoped coverage still proves project agents never receive this
  blocking operator diagnostic. Focused admin/MCP, downstream, both app
  feature configurations, hermetic curator, strict Clippy, formatting, diff,
  and dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed
  ignored and no model was started.
- Migrated typed `mission_new`, `mission_set_budget`, and `mission_set_pins`
  into the focused 230-line
  `corpus-admin/src/tools/missions/persistence.rs`; the admin facade fell from
  1,029 to 873 lines. All three are explicit unconfirmed,
  `missions`-audited, `missions`-refreshing writes.
- Creation still inherits project pins before applying exact mission
  overrides, normalizes an empty display name to absence, validates pins at
  authoring time, and preserves validation's unavailable-source fail-open
  behavior. Budget and full pin-map replacements retain their store ordering
  and output. Generated contracts now reject non-object maps, non-string pin
  values, and wrong optional string types before persistence. Existing curator
  inheritance, override, scope, validation and audit coverage plus focused
  schema/policy tests, downstream, both app feature configurations, hermetic
  curator, strict Clippy, formatting, diff, and dependency-policy gates remain
  green. Real Qwen3.8 MLX tests stayed ignored and no model was started.
- Migrated typed, origin-sensitive `mission_launch` into the focused 152-line
  `corpus-admin/src/tools/missions/launch.rs`; the admin facade fell from 873
  to 803 lines. The generated schema contains only project and mission
  identity—never a model-authored return address.
- Launcher-proven `MissionRunRef` still enters through dispatch context alone,
  must match the proven project, and is persisted exactly. Project, agent, and
  mission deletion guards, idempotent pending requests, unchanged briefs, and
  app-owned launch consumption remain intact. Existing scoped tests prove
  spoof resistance, scope injection, simultaneous curator return-address
  separation, and partial-origin failure; focused tests cover schema, matching,
  and the unconfirmed `missions` audit/refresh write policy. Downstream, both
  app feature configurations, hermetic curator, strict Clippy, formatting,
  diff, and dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed
  ignored and no model was started.
- Migrated confirmation-gated `mission_delete` into the focused 138-line
  `corpus-admin/src/tools/missions/delete.rs`; the admin facade fell from 803
  to 735 lines. Typed arguments reject a present non-string token before
  preview or mutation, and the definition is explicitly destructive,
  token-confirmed, `missions`-audited, and `missions`-refreshing.
- Dry-run agent/liveness/budget reporting and the operation/target-bound
  one-shot token remain unchanged. A confirmed safe mission deletes
  immediately; a guarded live mission clears launch intent, preserves teardown
  identity, records an idempotent durable delete request, and stays for the app
  to clean up. Focused schema/policy and summary tests plus existing scoped
  curator, token, store guard, app lifecycle, downstream, both feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Reduced the former 225-line mission domain to a 14-line composition facade.
  Immediate typed reads and their contract tests now live in the focused
  192-line `corpus-admin/src/tools/missions/read.rs`; shared live/status
  presentation lives in a private 48-line common owner. The admin facade fell
  from 735 to 712 lines.
- This cleanup is behavior-neutral: catalog order, generated schemas,
  dispatch, authorization, approval, audit, refresh, scoped behavior,
  lifecycle routing, and output remain unchanged. A direct boundary test now
  covers compact idle-duration formatting. Focused admin/MCP, full downstream,
  both app feature configurations, hermetic curator, strict Clippy,
  formatting, diff, and dependency-policy gates remain green. Real Qwen3.8 MLX
  tests stayed ignored and no model was started.
- Migrated typed `corpus_stats`, `corpus_list`, and `corpus_read` into the
  focused 251-line `corpus-admin/src/tools/corpus/read.rs` behind a 5-line
  corpus composition facade. The admin facade fell from 712 to 619 lines.
  Their generated definitions are explicit immediate, unconfirmed, unaudited,
  non-refreshing reads, and typed arguments now reject wrong project,
  category, and path types before filesystem observation.
- Sorted category listing, exact entry reads, knowledge/log stats separation,
  invalid-category output, schema generation, policy, and unknown-field
  compatibility now have direct tests. Existing path confinement, scoped
  project injection, catalog order, authorization, and output remain intact.
  Focused admin/MCP, full downstream, both app feature configurations,
  hermetic curator, strict Clippy, formatting, diff, and dependency-policy
  gates remain green. Real Qwen3.8 MLX tests stayed ignored and no model was
  started.
- Migrated structured `finding_list` into the focused 298-line
  `corpus-admin/src/tools/corpus/findings.rs`; the admin facade fell from 619
  to 505 lines. Its severity string-or-set union, default-unrated behavior,
  text filter, sort mode, and positive limit now share one generated typed
  contract and an explicit immediate, unconfirmed, unaudited, non-refreshing
  read policy.
- Valid query and JSON output behavior is unchanged, including recursive
  discovery, metadata warnings, duplicate-severity elimination, filtering,
  sorting, limits, and unknown-field compatibility. Wrong-typed optional
  `include_unrated`, `text`, and `sort` values now fail before discovery instead
  of silently selecting defaults; zero limits also fail before store access.
  Focused admin/MCP, scoped curator, full downstream, both app feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Migrated confirmation-gated `corpus_wipe` into the focused 153-line
  `corpus-admin/src/tools/corpus/wipe.rs`; the admin facade fell from 505 to
  468 lines. Typed arguments reject a present non-string token before corpus
  observation, and the generated definition is explicitly destructive,
  token-confirmed, `corpus`-audited, and `corpus`-refreshing.
- Dry-run file/byte and next-generation reporting, project-bound one-shot
  tokens, generation advancement, and output remain unchanged. Direct coverage
  now proves confirmed wiping removes corpus contents while the project and
  its agents survive. Existing token replay/op-scope, scoped Super authority,
  app revision/invalidation, full downstream, both feature configurations,
  hermetic curator, strict Clippy, formatting, diff, and dependency-policy
  gates remain green. Real Qwen3.8 MLX tests stayed ignored and no model was
  started.
- Migrated confirmation-gated `entry_delete` into the focused 219-line
  `corpus-admin/src/tools/corpus/entries/delete.rs` behind a 5-line entry
  lifecycle facade. The admin facade fell from 468 to 335 lines. Typed
  arguments reject wrong recursive and confirmation-token types before path
  resolution, and the generated definition is explicitly destructive,
  token-confirmed, `corpus`-audited, and `corpus`-refreshing.
- Canonical confinement, sorted metadata-only previews, recursive-tree
  opt-in, and operation/path/mode/fingerprint-bound tokens retain their order
  and output. Direct tests cover deterministic preview counts/fingerprints and
  change sensitivity. Existing stale-token, recursive-tree, immutable-runs,
  traversal/symlink, scoped curator, audit, full downstream, both feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Migrated typed `entry_move` into the focused 168-line
  `corpus-admin/src/tools/corpus/entries/move.rs`; the admin facade fell from
  335 to 306 lines. Its generated contract rejects a present non-boolean
  overwrite flag before path resolution, and the definition is explicitly an
  unconfirmed, `corpus`-audited, `corpus`-refreshing write.
- Direct tool coverage proves exact output, missing-parent creation,
  destination collision refusal, and explicit overwrite. Existing
  same-project source/destination confinement, bare-category and immutable-runs
  guards, traversal/symlink resistance, scoped policy, full downstream, both
  feature configurations, hermetic curator, strict Clippy, formatting, diff,
  and dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed
  ignored and no model was started.
- Migrated typed `entry_write` into the focused 140-line
  `corpus-admin/src/tools/corpus/entries/write.rs`; the admin facade fell from
  306 to 259 lines. Its generated contract requires string project, path, and
  content fields while retaining unknown-field compatibility, and its
  definition is explicitly an unconfirmed, `corpus`-audited,
  `corpus`-refreshing write.
- Direct tool coverage proves exact UTF-8 byte output, missing-parent creation,
  and replacement in place. Existing store-level atomic writes, category and
  canonical-path confinement, immutable-runs and traversal/symlink guards,
  scoped curator injection, and audit behavior remain green. The typed
  registry now exclusively owns all 35 catalog entries and dispatch routes;
  no handwritten catalog or fallback handler remains. Focused admin/MCP, full
  downstream, both app feature configurations, hermetic curator, strict
  Clippy, formatting, diff, and dependency-policy gates are green. Real
  Qwen3.8 MLX tests stayed ignored and no model was started.
- Moved shared project/clock adaptation into the private 21-line
  `corpus-admin/src/common.rs` module and the complete destructive confirmation
  protocol into the focused 173-line `corpus-admin/src/confirmation.rs` owner.
  The public admin facade fell from 259 to 170 lines and is limited to state,
  tool-set/catalog projection, and dispatch responsibilities.
- `PendingConfirm` remains nameable by the scoped adapter but its operation,
  target, and expiry state is opaque. Direct tests prove exact confirmation
  output and consumption after success, replay, mismatch, expiry, and failed
  mutation. Existing five-operation classification, 60-second TTL, target
  binding, lifecycle routing, authorization, full downstream, both app feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Added the focused 181-line `corpus-cli/src/cli.rs` typed Clap boundary and
  reduced the CLI entry point from 552 to 462 lines. Clap 4.6.6 now generates
  the top-level command tree, help, and typed `run` parser; it was already in
  the lockfile, so this direct dependency added no locked package. The manual
  usage document, top-level string dispatch, and run option loop are gone.
- Typed parsing requires agent and non-empty multiword mission values and
  validates model/research options before store or model work. Transitional
  passthrough preserves exact argv for unconverted command domains. Unit and
  binary tests cover option placement, missing/unknown arguments, generated
  headless help, the installed `corpus` name, legacy error prefix/failure exit,
  and validation ordering. Existing resource-root execution, full downstream,
  both app feature configurations, hermetic curator, strict Clippy, formatting,
  diff, and dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed
  ignored and no model was started.
- Migrated `models` and all nine `plugin` operations to generated Clap command
  trees. Their behavior now lives in focused 31-line model and 182-line plugin
  owners behind a 4-line composition module; the CLI entry point fell from 462
  to 283 lines.
- Catalog validation, bundle verification, install/select output, lifecycle
  progress, 30-minute setup and two-minute management deadlines, ten-second V1
  probe, legacy probe behavior, V1/legacy raw calls, optional JSON parameters,
  and model resource resolution remain unchanged. Pure, parser, and binary
  tests cover deadlines, JSON, typed forms, validation before discovery,
  generated nested help, and out-of-workspace resources. Focused CLI, full
  downstream, both app feature configurations, hermetic curator, strict
  Clippy, formatting, diff, and dependency-policy gates remain green. Real
  Qwen3.8 MLX tests stayed ignored and no model was started.
- Migrated all six project operations to a generated `ProjectCommand` tree and
  focused 102-line handler owner. The transitional store-admin module fell
  from 763 to 635 lines. Clap now owns the new-project plugin default, clone
  destination/name/corpus flags, rebind input, required values, and nested
  help.
- List/output behavior, name fallback, cloning, clean or lifecycle-requested
  deletion, generation-advancing corpus wipe, rebind, and store validation are
  unchanged. Parser tests cover defaults and required/complete forms. An
  isolated binary campaign covers create/list/clone/rebind/wipe/delete and
  final projection; another proves invalid clone input creates no data root.
  Existing live-mission lifecycle coverage, full downstream, both app feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Migrated all six agent operations to a generated `AgentCommand` tree and
  focused 156-line handler owner. The transitional store-admin module fell
  from 635 to 456 lines. Clap now owns required project/agent targets, the
  `researcher` creation default, clone destination, optional role get/set
  value, migration apply mode, and nested help.
- Role validation reuses the core `AgentRole` parser and advertised names.
  List/output behavior, cloning, clean or lifecycle-requested deletion, and
  dry-run-by-default role migration are unchanged. Parser tests cover defaults
  and required/invalid forms. An isolated binary campaign covers
  create/list/role-get/role-set/clone/migrate/delete and final projection;
  another proves an invalid role creates no data root. Existing live-mission
  lifecycle coverage, full downstream, both app feature configurations,
  hermetic curator, strict Clippy, formatting, diff, and dependency-policy
  gates remain green. Real Qwen3.8 MLX tests stayed ignored and no model was
  started.
- Migrated all three mission operations to a generated `MissionCommand` tree
  and focused 191-line handler owner. The transitional store-admin module fell
  from 456 to 311 lines. Clap now owns project/mission targets, required agent
  and multiword brief values, optional budget, repeatable typed
  `source=revision` overrides, deletion targets, and nested help.
- Mission pin construction retains and directly tests the precedence order:
  plugin defaults, then stored project pins, then mission overrides. Existing
  revision validation, list/output behavior, Markdown brief storage, clean
  deletion, and lifecycle-requested deletion are unchanged. Parser tests cover
  complete, required, and malformed forms. An isolated binary campaign covers
  project/agent setup, mission creation/listing, budget, repeated pins, exact
  brief storage, clean deletion, and durable `delete_requested` retention for
  a live session; another proves malformed pins create no data root. Existing
  lifecycle coverage, full downstream, both app feature configurations,
  hermetic curator, strict Clippy, formatting, diff, and dependency-policy
  gates remain green. Real Qwen3.8 MLX tests stayed ignored and no model was
  started.
- Migrated finding list/show to a generated `FindingCommand` tree and focused
  97-line handler owner. The transitional store-admin module fell from 311 to
  144 lines. Clap now owns project/path targets, repeatable and comma-separated
  core severity values, unrated exclusion, optional text search, typed sorting,
  positive limits, and nested help.
- Core severity parsing and confined exact-read paths remain the authorities.
  Table/warning formatting, tolerant unrated projection, in-memory query
  behavior, empty results, and exact body output are unchanged. Parser and
  projection tests cover complete queries, deduplication, defaults, and invalid
  values. An isolated binary campaign covers rated/unrated discovery, combined
  filters, sorting, limits, text, empty results, exact reads, and traversal
  refusal; another proves invalid severity creates no data root. Existing
  finding-contract and confinement coverage, full downstream, both app feature
  configurations, hermetic curator, strict Clippy, formatting, diff, and
  dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed ignored
  and no model was started.
- Migrated audit and refusal readers to typed `AuditArgs`/`RefusalsArgs` and a
  focused 88-line operator-log owner. Both retain the 50-record tail default;
  refusal filtering now parses to the core seven-variant `Gate` authority.
  The remaining 144-line store-admin module and transitional passthrough type
  were deleted, so every headless command now enters through typed Clap data.
- Tail-before-filter behavior, oldest-first formatting, three-line detail
  limits, outcome/target output, refusal role/run/argument diagnostics, and
  positive empty-log explanations are unchanged. Parser tests cover defaults,
  explicit values, and invalid forms. An isolated binary campaign covers empty
  logs, audit tailing/detail truncation, refusal gate filtering and diagnostic
  fields; another proves invalid gates create no data root. Existing
  append-only and out-of-project log coverage, full downstream, both app
  feature configurations, hermetic curator, strict Clippy, formatting, diff,
  and dependency-policy gates remain green. Real Qwen3.8 MLX tests stayed
  ignored and no model was started. Phase 5 is complete.

## Phase 6 entry and direct image cleanup

- The measured UI entry hotspots are `views/projects.rs` at 1,821 lines,
  `views/agents.rs` at 1,351, `sidebar.rs` at 1,281, and `chat/panel.rs` at
  1,223. The full dependency and observability baseline is recorded in
  [`phase-6-inventory.md`](phase-6-inventory.md).
- Production has no structured tracing subscriber at entry. Lifecycle and
  delivery coordinators are the first planned seams; paint functions remain
  outside the operational event boundary. Syntect's Oniguruma backend and the
  deprecated YAML serialization boundary require independent compatibility
  campaigns. Goose remains untouched.
- Removed Corpus's direct workspace/app `image 0.25` dependency and replaced
  its single icon-decoding call with eframe 0.31.1's exact PNG adapter. A
  regression fixes the bundled icon at 250 by 250 pixels and verifies a
  complete RGBA buffer. The offline lock refresh pruned 41 package entries and
  454 lines while retaining image versions required transitively by UI crates
  and Goose.
- Strict Clippy, the full selected-workspace tests, default and
  no-default-feature app gates, dependency policy, formatting, and diff checks
  pass. Both real Qwen3.8 MLX scenarios stayed ignored and no model was
  started. Phase 6 remains in progress; the next slice extracts the
  finding-summary responsibility from `views/projects.rs`.
- Extracted finding projection and presentation into a focused 250-line,
  opaque component. Its public-to-views surface is limited to construction,
  visibility, and rendering; severity counts and discovery-state variants do
  not leak back into the project dashboard. Loading/failure/last-good behavior,
  all severity counts including unrated, zero-count omission, and responsive
  tile sizing retain their four focused regression tests.
- `views/projects.rs` fell from 1,821 to 1,591 lines and no longer imports
  finding discovery or severity types. Strict Clippy, full downstream tests
  including the hermetic curator campaign, both app feature configurations,
  dependency policy, formatting, and diff checks pass. The real Qwen3.8 MLX
  tests remained ignored and no model was started. The next slice defines the
  structured tracing contract and instruments its first lifecycle boundary.
- Added the typed `corpus.lifecycle` / `lifecycle.operation` event contract.
  Its fixed fields are project, mission, run-session identity, operation,
  generation, elapsed milliseconds, outcome, retryability, and error. The
  asynchronous launch-adoption coordinator emits one terminal event, and it
  derives retryability from retained `RunPhase` state rather than error text.
- A test-only capture layer locks the complete successful field map. The
  existing deletion-during-adoption campaign now also proves the real failure
  event while retaining its stop/cleanup assertions. Direct `tracing 0.1.44`
  and test-only `tracing-subscriber 0.3.23` reuse versions already resolved in
  the lockfile; Goose remains untouched. Strict Clippy, full downstream tests
  including hermetic curator orchestration, both app feature gates, dependency
  policy, formatting, and diff checks pass. Real Qwen3.8 MLX probes stayed
  ignored and no model was started. Production subscriber installation is the
  next bounded observability slice.
- Installed the production subscriber before app state and chat startup. It
  accepts only `corpus.lifecycle`, writes flattened JSONL under
  `<CORPUS_HOME>/var/diagnostics`, uses a bounded non-blocking queue, rotates
  daily, and retains eight matching files. The writer guard spans the GUI
  process and flushes queued events on shutdown.
- Diagnostic-directory and global-subscriber failures produce one startup
  warning and do not prevent the application window from opening. Tests prove
  retention preserves unrelated files, an unavailable sink returns an error
  rather than panicking, and the executable degrades that error to `None`.
  Direct `tracing-appender 0.2.5` and production `tracing-subscriber 0.3.23`
  reuse locked packages; Goose remains untouched. Strict Clippy, full
  downstream tests including hermetic curator orchestration, both app feature
  gates, dependency policy, formatting, and diff checks pass. Real Qwen3.8 MLX
  probes stayed ignored with no model started. Curator completion delivery is
  the next instrumented operation.
- Added `corpus.delivery` terminal events for curator completion prompts. The
  fixed fields include the launcher-proven parent project/mission/run, exact
  deterministic message id, attempt, grouped child count, elapsed time,
  outcome, terminal state, retryability, and error. Quiet admission/polling
  states emit nothing; acknowledgement, failure, retry readiness, persistence
  failure, and status observation errors are explicit. The production sink
  allowlist now accepts both lifecycle and delivery targets.
- The new seam exposed ignored persistence results after acknowledgement and
  retry release. The reconciler now checks every grouped child update. A stale
  durable identity produces a visible, retryable `persistence_failed` event
  instead of false success. Tests cover exact grouped acknowledgement message
  identity, non-retryable model failure, retry after model switch, and a
  persistence race. Strict Clippy, full downstream tests including the
  hermetic curator campaign, both app feature modes, dependency policy,
  formatting, and diff checks pass. Real Qwen3.8 MLX probes remained ignored
  and no model was started. Syntect backend compatibility is the next
  dependency slice.
- Replaced Syntect's native `regex-onig` backend with its supported pure-Rust
  `regex-fancy` backend and removed the redundant explicit `parsing` feature.
  The resolved graph adds Syntect's required `fancy-regex 0.16.2` but removes
  `onig` and `onig_sys`, so Corpus no longer builds or links Oniguruma. Goose's
  separately constrained `fancy-regex` 0.17/0.19 packages remain untouched and
  cannot be unified with Syntect's current requirement.
- Expanded syntax compatibility coverage to representative editable Markdown
  and agent JSON configuration, including fenced code, links, strings,
  numbers, booleans, and punctuation. Source and palette contracts pass under
  both regex engines; the final pure-Rust configuration passes both app feature
  modes. In an unoptimized process the cold focused suite moves from about
  0.04 to 0.69 seconds, while a temporary probe
  measured 100 post-initialization uncached edits at about 124 milliseconds
  total; the probe was then removed. Strict Clippy, full downstream tests
  including the hermetic curator campaign, dependency policy, formatting, and
  diff checks pass. Real Qwen3.8 MLX probes remained ignored and no model was
  started. Persisted-YAML compatibility design is the next dependency slice.
- Added the store-owned `yaml` adapter and routed every production YAML read,
  write, representation, and error through it. The adapter exposes stable
  one-based locations without leaking the backend error type. Observe now uses
  that seam for the shipped model registry; direct YAML edges were removed from
  observe, core's tests, and integration. The integration edge was dead because
  its scenario YAML is copied as evidence but never parsed. Corpus now has one
  direct `serde_yaml` owner in store; Goose's transitive use is untouched.
- Added scalar/type, nested-map, unknown-field, and malformed-location adapter
  tests plus production model-registry compatibility coverage. Documented the
  complete persisted-surface matrix, researched candidate order, and acceptance
  gate in [`yaml-compatibility.md`](yaml-compatibility.md). `yaml_serde 0.10` is
  first for a compatibility trial; `serde-saphyr` remains the stronger but more
  behaviorally risky hardening candidate. No replacement was selected in this
  slice. Strict Clippy, full downstream tests including the hermetic curator
  campaign, both app modes, dependency policy, formatting, and diff checks
  pass. Real Qwen3.8 MLX probes remained ignored and no model was started.
- Replaced Corpus's archived YAML backend with `yaml_serde 0.10.7` behind the
  store adapter. The pre-swap backend's representative bytes, rendered-role
  fixtures, and finding output were captured and passed identically after the
  swap; scalar, unknown-field, location, persisted-store, and shipped-registry
  behavior also remained unchanged. `yaml_serde` and `libyaml-rs 0.3.0` now sit
  solely below store, while deprecated `serde_yaml` remains only through
  untouched Goose.
- This is a maintenance improvement, not an unsafe-code elimination:
  `libyaml-rs` is documented as a C2Rust translation of libyaml and the Serde
  layer has narrow unsafe internals. `serde-saphyr` remains the possible future
  pure-Rust/budgeted migration. The full gate passes, real Qwen3.8 MLX probes
  remained ignored, and no model was started. Enforceable supply-chain policy
  is the next Phase 6 slice.
- Completed Phase 6 with an executable supply-chain policy pinned to
  `cargo-deny 0.20.2`. CI and the local wrapper now gate RustSec advisories,
  yanked packages, licenses, registry/Git provenance, full Git revisions, and
  exact reviewed duplicate-version exceptions across Linux and Apple Silicon.
- The initial audit remediated `RUSTSEC-2026-0258` by updating `h2` 0.4.15 to
  0.4.16. Two `quick-xml 0.30.0` denial-of-service advisories remain documented
  exceptions on Egui's Linux accessibility path with a 2026-11-26 review date
  and an upstream-removal trigger. Goose remains untouched and is the sole
  permitted Git source at its existing full revision.
- Strict Clippy, the full downstream suite including the hermetic curator
  campaign, both app feature configurations, package and supply-chain policy,
  formatting, and diff checks passed. Real Qwen3.8 MLX tests remained ignored
  and no model was started.

## Phase 7 architecture checkpoint — 2026-08-26

- Added [`architecture.md`](architecture.md) as the source of truth for shipped
  executable composition, workspace dependencies, major module ownership,
  runtime flows, data roots, project-only execution namespaces, and curator
  completion delivery. README navigation now points to tracked documentation
  instead of machine-local `dev/` scratch.
- Expanded `scripts/check-dependency-policy` to enforce the exact normal
  workspace graph for all ten packages before checking the host-admin
  transitive boundary. Package inventory, missing edges, and unexpected edges
  now fail with explicit expected/actual diagnostics.
- The architecture graph, host-admin boundary, formatting, and diff checks
  pass. This slice changed no runtime behavior, kept live Qwen3.8 MLX tests
  ignored, and started no model.
- Promoted the Phase 1 threat-model baseline into the final shipped security
  contract. Stable `SEC-*` identifiers now cover proven identity, role and
  operator authority, filesystem confinement/publication, durable cleanup and
  stale-result rejection, bounded process/protocol behavior, audit custody,
  and global MLX test serialization. Trust assumptions and explicit
  non-guarantees distinguish host-trusted plugins from untrusted model and
  protocol data.
- Every invariant links its enforcement owner and representative regression
  evidence. The focused store/core/admin/MCP/headless-app/integration gate
  passed 416 tests with four classified live/platform tests ignored;
  formatting and diff checks also pass. No model was started.
- Split the 597-line mixed-audience `AGENTS.md` into a 183-line contributor
  contract plus tracked [`operator-guide.md`](operator-guide.md) and
  [`troubleshooting.md`](troubleshooting.md) owners. Routine setup, curator
  orchestration, chat, destructive confirmation, durable cleanup, audit, and
  upgrade guidance now has an operator home; refusal, launch, attach, plugin,
  chat, model, build, and escalation diagnosis is organized by symptom.
- README and the architecture documentation map route to the new guides while
  `testing.md` and `PLUGINS.md` remain authoritative. Documentation links, CLI
  contracts, dependency policy, formatting, and diff checks pass. This was a
  documentation-only slice; no live Qwen3.8 MLX test ran and no model started.
- Added tracked [`decisions.md`](decisions.md) with the durable what/why/where
  rationale formerly mixed into ignored, version-stale local plans. The local
  roadmap and TODO now contain only current work; eight completed/superseded
  plans and spike artifacts were removed from `dev/` after consolidation.
- Removed shipped-source references to missing local plans and stale phase,
  chunk, and future-extraction commentary. Strict headless Clippy passes; 276
  focused app/CLI/core/observe/integration tests pass with five classified
  platform/live tests ignored. Documentation links, dependency policy,
  formatting, and diff checks pass. No model was started.
- Completed the repository-wide closeout gate: the locked ten-package workspace
  builds, the default-feature all-target suite passes 474 tests with eight
  classified ignores, and strict workspace Clippy, formatting, diff,
  architecture, documentation-link, and `cargo-deny 0.20.2` checks pass. GitHub
  Actions are pinned to full commits. The model workflow now runs the embedded
  library probes and full Curator scenario serially.
- The serial live `qwen3.8:27b-mlx` gate passes one configured-model smoke test,
  three embedded management-chat tests, and the complete two-child Curator
  campaign. Its first campaign attempt exposed a real fixture boundary: an
  existing tmux server did not inherit `CORPUS_PLUGINS_DIR`, so the worker
  remained active retrying an unavailable fixture plugin and no completion was
  fabricated. Run-local MCP config now propagates that explicit catalog
  override, and the scenario checks it before inference. The corrected campaign
  launched children one at a time, delivered both exact completion notices,
  survived coordinator reconstruction, and passed in 169.76 seconds. Goose is
  untouched at its existing full revision. Phase 7 is complete.
