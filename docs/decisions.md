# Corpus architectural decisions

This is the tracked record of durable decisions that explain the shipped
architecture. Temporary investigations and active implementation plans belong
in the ignored `dev/` directory; when completed, their lasting outcome is
summarized here and the temporary artifact is removed.

The current system shape is authoritative in [`architecture.md`](architecture.md),
security requirements in [`threat-model.md`](threat-model.md), and verification
policy in [`testing.md`](testing.md). This record explains why those boundaries
exist without retaining obsolete chunk plans, branch names, line numbers, or
dependency versions.

## Identity-bound run lifecycle

**Decision.** Every run operation is bound to project, mission, and unique run
generation. Preparation and pre-spawn work are cancellable; late results are
rejected when their identity no longer matches. A spawned process is adopted
before fallible persistence, and failed persistence triggers owned cleanup.
Stop reports evidence export separately from environment cleanup and retains
durable identity until teardown is proven.

**Why.** Selection, display labels, process ownership, and durable session
identity are not interchangeable. Treating them as one value allowed project
switches and late background work to affect a newer run, and allowed teardown
failures to disappear.

**Where.** `corpus-app/src/state/run/`, `corpus-app/src/state/session.rs`,
`corpus-core/src/launch/`, and `corpus-store` mission/environment records.
Security requirements are `SEC-ID-3`, `SEC-DUR-1`, and `SEC-DUR-2`.

## Paint is rendering, not an I/O scheduler

**Decision.** The Egui update path reads prepared state and paints it. Plugin,
source, model, session, launch, teardown, and corpus work runs through typed
background jobs with operation identity, deadlines, cancellation, terminal
results, duplicate suppression, stale-scope rejection, and an explicit repaint
wakeup after delivery. Streams and visible histories have explicit bounds.

**Why.** Moving a blocking call to an arbitrary thread does not solve freezes,
orphan work, unobservable failure, or stale result races. The job contract makes
those outcomes values that can be tested.

**Where.** `corpus-app/src/jobs.rs`, `corpus-app/src/state/background.rs`, and
the domain coordinators below `corpus-app/src/state/`. Views request actions and
never perform unbounded filesystem, network, plugin, or process work.

## Runtime facilities have deterministic seams

**Decision.** Time, owned runs, session discovery/control, filesystem
notifications, and job spawning sit behind Corpus-owned interfaces. Production
adapters are thin; hermetic tests inject deterministic implementations.
Filesystem events are bounded invalidation hints that trigger authoritative
reconciliation, never proof of a run transition.

**Why.** Races, deadlines, restarts, process exit, and watcher failure cannot
be tested reliably through ambient clocks, `PATH` mutation, or real tmux. File
events also lack the identity needed to attribute project-wide writes to one
mission.

**Where.** `corpus-app/src/jobs.rs`, `file_watch.rs`, `session_service.rs`, and
the state coordinators. The real tmux path remains a separately classified
platform test.

## OpenCode HTTP is an optional bounded adapter

**Decision.** An explicitly configured password-protected loopback OpenCode
server may accelerate session and message reads behind `SessionService`.
Corpus validates every returned directory and session identity against its own
run records. Incompatibility falls back to the off-thread CLI adapter. Corpus
does not depend on cross-process OpenCode events for liveness or reconciliation.

**Why.** The compatibility investigation found fast HTTP reads but permissive
ID lookup and no useful replayable cross-process event stream. The useful read
path did not justify transferring authority or lifecycle ownership to the
server.

**Where.** `corpus-app/src/session_service.rs`; configuration is explicit via
`CORPUS_OPENCODE_SERVER_URL` and `CORPUS_OPENCODE_SERVER_PASSWORD`.

## Administration and research are separate artifacts

**Decision.** Host administration uses `corpus-admin-mcp`, backed by
`corpus-admin`, `corpus-store`, and `corpus-observe`. It cannot link research,
plugin execution, source preparation, launch, app, or Goose capabilities.
Research and project management use `corpus-mcp`; Curator and Super receive a
catalog filtered by server-derived role and injected project scope.

**Why.** An argv flag on one all-capabilities process is not a trust boundary.
Artifact composition and an exact dependency graph make accidental authority
growth reviewable and executable.

**Where.** The workspace graph in `architecture.md`, the catalogs and handlers
in `corpus-admin`, adapters in `corpus-mcp`, and
`scripts/check-dependency-policy`. Security requirements are `SEC-AUTH-1`
through `SEC-AUTH-4`.

## Super is project-local, not host-global

**Decision.** Super combines Researcher, Tester, and scoped Curator authority
inside one proven project. It may confirmation-gated wipe that project's
corpus, but cannot create, clone, rebind, or delete projects; access another
project; copy across projects; grant itself ambient shell; or inspect benchmark
and plugin internals. Curator may perform necessary project-local cleanup but
cannot create or promote Super agents.

**Why.** A useful project coordinator needs the union of project capabilities,
not host administration. Scope injection, role ceilings, destructive
confirmation, and mandatory audit preserve that distinction.

**Where.** `corpus-store/src/agents/`, `corpus-admin`, `corpus-mcp`, rendered
role fixtures, and the `SEC-AUTH-*` invariants.

## Environment plugins are external, immutable, and retained

**Decision.** Corpus owns the language-neutral `corpus.environment/1`
vocabulary, immutable installation and selection, source/runtime custody,
durable environment leases, bounded process protocol, and exact evidence
provenance. Production adapters live in independently released plugin
repositories. Corpus keeps conformance fixtures, not editable first-party
production adapters. Goose is likewise retained behind a Corpus-owned adapter
until its intended crate distribution is deliberately available.

**Why.** Environment-specific Docker topology and oracles must evolve without
forking Corpus, while runs remain reproducible and upgrades cannot silently
rewrite old evidence. Removing Goose during unrelated cleanup would discard a
chosen future integration path rather than simplify an owned boundary.

**Where.** `PLUGINS.md`, `corpus-core` plugin/install/session modules,
`corpus-observe` discovery, `corpus-store` environment records, protocol
fixtures, and the `corpus-app/src/chat/` Goose quarantine.

## Embedded management chat owns its team and approval policy

**Decision.** The desktop app embeds Goose in-process behind Corpus-owned
`Chat`, event, and command types. Operator sessions receive the full admin
catalog. Orchestrator holds no admin extension and delegates through a
Corpus-owned frontend tool to independently scoped specialist agents. Read
tools auto-release, ordinary writes require approval by default, destructive
tools always require approval, and unknown tools fail closed.

**Why.** Goose delegation inherits only parent extensions and therefore cannot
grant a specialist tools that a tool-less orchestrator lacks. Corpus-owned
delegation preserves least privilege. Per-turn cancellation, serialized sends,
bounded histories, structured transcripts, and explicit diagnostics address
the lifecycle failures observed in the original harness.

**Where.** `corpus-app/src/chat/embedded.rs`, `team.rs`, `panel.rs`, and
`mod.rs`; server confirmation remains defense in depth in `corpus-admin`.

## Curator orchestration is event-driven and exact-origin

**Decision.** Curator and Super may launch several child missions and finish
their turn. Corpus observes children without inference and delivers completed,
failed, or unexpectedly exited results once to the exact persisted parent
project, mission, run, control endpoint, and OpenCode conversation. Ready
results for one parent are grouped. Delivery is queued behind an active parent
turn and survives restart.

**Why.** Polling or keeping a model turn open wastes inference and still cannot
prove the recipient. Agent name, role, newest session, terminal quiet, and
timers are not return addresses or completion evidence.

**Where.** Typed mission dispatch records in `corpus-store`, scoped launch
origin in `corpus-mcp`, delivery/reconciliation in
`corpus-app/src/state/dispatch.rs`, and private loopback session control in
`corpus-app/src/session_service.rs`. The complete serial MLX campaign is in
`corpus-integration` and `docs/testing.md`.

## Persisted data changes pass through narrow adapters

**Decision.** Store modules own atomic replacement, path confinement,
frontmatter/YAML serialization, and compatibility behavior. YAML uses
`yaml_serde` behind the store adapter; callers do not select a backend. Unknown
and legacy records follow the documented compatibility contract rather than
being opportunistically rewritten.

**Why.** Central ownership lets characterization fixtures protect exact bytes
and semantics while dependencies change. It also prevents each caller from
inventing unsafe publication or path handling.

**Where.** `corpus-store/src/filesystem.rs`, `frontmatter.rs`, `yaml.rs`, domain
modules, and [`yaml-compatibility.md`](yaml-compatibility.md).

## Dependencies are capabilities with explicit custody

**Decision.** Every direct dependency has an owning module and concrete job.
Workspace edges are exact, supply-chain policy is executable, Git sources are
full-revision pinned, and exceptions are narrow, dated, and carry a removal
trigger. New crates or libraries must delete meaningful custom machinery,
enforce a boundary, or provide a measured maintenance/security improvement.

**Why.** Fewer manifest entries alone do not make a system simpler. Authority
direction, duplicate versions, source provenance, and obsolete custom code are
the properties that affect review and maintenance.

**Where.** `Cargo.toml`, `deny.toml`, `scripts/check-dependency-policy`,
`scripts/check-supply-chain`, [`architecture.md`](architecture.md), and
[`supply-chain-policy.md`](supply-chain-policy.md).

