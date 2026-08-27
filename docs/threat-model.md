# Corpus security invariants and threat model

Status: shipped security contract after the senior-developer refactor  
Last verified: 2026-08-26

This document defines the authorities Corpus keeps separate, the identities it
must prove, and the failure behavior required below model-facing schemas. The
system composition is documented in [`architecture.md`](architecture.md), test
tiers in [`testing.md`](testing.md), and dependency exceptions in
[`supply-chain-policy.md`](supply-chain-policy.md).

Stable invariant IDs are review handles. Moving code is safe only when the same
invariant still has one enforcing owner and equivalent regression evidence.

## Security objective

Corpus runs offensive-capable research against operator-authorized projects.
It must prevent model output, a research agent, stale UI state, a plugin reply,
or a concurrent lifecycle event from silently widening:

- project, role, mission, run, model, source, or environment identity;
- filesystem or process reach;
- destructive authority;
- curator completion routing;
- durable cleanup and audit custody.

The preferred failure is a visible refusal that preserves prior durable state
and enough exact identity to retry or clean up safely.

## Trust assumptions and limits

Trusted:

- the operator, operating-system account, and local host;
- a plugin bundle the operator deliberately installs and selects;
- the Corpus binaries and shipped read-only resources;
- configured local model transport only for availability and confidentiality,
  never for authorization decisions.

Untrusted or adversarial:

- model text, tool arguments, permission requests, and generated paths;
- agent-authored corpus content, prompts, findings, and proof-of-concept code;
- target behavior and sandbox output;
- plugin protocol replies until validated against the selected manifest and
  durable environment identity;
- repository contents, symlinks, malformed records, stale processes, and
  concurrent filesystem changes;
- UI selection and cached background results after their captured generation.

Corpus does not defend against a malicious local user with the same filesystem
permissions, a compromised host/kernel, or an intentionally malicious plugin
the operator has trusted. Plugins execute host-side orchestration and define
the actual sandbox/egress controls; the generic protocol cannot turn a hostile
plugin into a security boundary. Corpus also does not currently claim built-in
store encryption, signature verification, or complete denial-of-service
resistance for arbitrarily large operator-owned data.

## Protected assets

- project corpus entries, findings, agents, missions, usage, and transcripts;
- launcher-proven project, agent, mission, generation, run, and parent origin;
- explicit model and immutable source revisions;
- role catalogs and rendered OpenCode permissions;
- audit/refusal logs outside project-writable trees;
- one-shot destructive confirmation intent;
- installed plugin/source caches and durable environment cleanup records;
- curator message identity, acknowledgement, and retry state;
- local model capacity and first-attempt integration evidence.

## Authority zones

| Zone | Authority | Inputs treated as untrusted | Hard boundary and failure policy |
|---|---|---|---|
| Host operator UI/CLI | May select and administer local projects | UI state, typed CLI values, model-generated admin calls | Re-resolve durable target state; reject stale generations and invalid typed input |
| Embedded Goose adapter | Management conversation only | Model text, tool calls, approval decisions, stream events | Corpus-owned chat types and approval gate; adapter error ends the turn without mutation |
| Host admin MCP | Full operator administration | Model tool arguments | Typed registry, audit classification, exact target, confirmation where destructive; never gains plugin/launch powers |
| Project-scoped research MCP | One proven project/run and server-derived role | All agent arguments and plugin replies | Inject scope/origin, advertise a role-filtered catalog, and refuse uniformly when identity is partial |
| Curator domain | Manage agents, missions, and corpus inside one project | Child requests, completion state, attempted cross-project arguments | No sandbox or open research authority; every mutation audited; cannot become host-global operator |
| Tester domain | Execute and publish verified work in the selected sandbox | Target and sandbox output | No internet research or project administration; server role gate remains authoritative |
| Researcher domain | Internet research without execution | External content | No sandbox, finding publication, or project management |
| Super domain | Union of project-scoped curator/tester/researcher powers | Same inputs as all project roles | Still confined to one proven project; no host-global lifecycle or unrestricted shell |
| OpenCode/tmux process | Run one explicit launch plan | Mission/prompt, rendered agent, terminal/session output | Project-only cwd, explicit environment, bounded supervision, exact session identity |
| Plugin process and sandbox | Host-trusted environment orchestration and target-specific tools | Manifest and streamed protocol data; research code inside sandbox | Version/capability negotiation, deadlines, output caps, kill/reap; plugin owns sandbox isolation |
| Store and caches | Durable local truth | Paths, records, concurrent writers, symlinks | Checked transitions, no-follow confinement, collision-safe atomic/exclusive publication |
| Local model test runner | One Qwen3.8 MLX inference at a time | Nondeterministic model behavior | Exact model/digest preflight and cross-process lease held through evidence capture |

The host admin MCP and project research MCP are separate executables. Their
shared typed tool definitions do not imply shared authority: the scoped adapter
removes model-supplied project fields and injects launcher-proven identity.

## Identity invariants

### SEC-ID-1: project scope is proven, never inferred

Every project-scoped MCP call uses the validated `CORPUS_PROJECT` launch scope.
The server does not use an argument, current UI selection, working-directory
guess, or default project. A missing/deleted project makes all scoped tools
fail before a write can recreate it.

Enforcement: `corpus-mcp::tools::Ctx::from_env`, `write_scope`, and
`scoped_management_dispatch`; store slug and project existence checks.

Evidence: `scope_gate.rs`, `curator.rs::a_curator_cannot_reach_another_project`,
`curator.rs::an_unresolved_scope_refuses_every_management_tool`, and
`run_workspace.rs::a_run_dir_exposes_only_its_own_project`.

### SEC-ID-2: mission origin and completion return address are exact

A curator cannot author `requested_by`. Mission launch origin comes from the
proven project/mission/run pair in the launcher environment. Child completion,
message admission, terminal delivery, and acknowledgement match the full
parent and child run identities plus deterministic message identity.

Enforcement: scoped admin dispatch, mission launch validation, durable mission
dispatch records, `state/dispatch.rs`, and `session_service.rs`.

Evidence: `mission_launch::launcher_origin_must_match_the_proven_project`,
`curator.rs::simultaneous_curator_missions_keep_distinct_return_addresses`,
`state/tests/delivery.rs`, and the hermetic full curator campaign.

### SEC-ID-3: launch identity is immutable and explicit

One launch plan owns project, agent, mission, model, source pins, environment
session, generation, and transcript intent. The child environment derives from
that plan. A launch without an explicit agent model or registry tool-use model
fails rather than inheriting OpenCode's ambient default.

Enforcement: `corpus-core::launch::{plan,policy,command,start}` and source-pin
preparation before process construction.

Evidence: launch construction/policy tests, including
`plan_owns_every_launch_identity_input`,
`backend_specific_inputs_cannot_overlap`, and
`both_launch_paths_export_the_agent_identity_and_tui_session`.

## Authorization invariants

### SEC-AUTH-1: the server catalog is the capability authority

`AgentRole::{Super, Curator, Tester, Researcher}` determines the MCP catalog at
server startup from a proven project agent. Unresolved role denies all tools.
Rendered OpenCode permissions are defense in depth, not the authority.

Evidence: `curator.rs::a_curator_advertises_exactly_its_grant_set`,
`scoped_writes.rs::advertised_catalog_matches_the_role`, and
`scoped_writes.rs::unresolved_role_denies_every_tool`.

### SEC-AUTH-2: stored permissions may tighten but never widen a role

Agent rendering starts from a total typed policy. Stored permissions can
remove capabilities but cannot restore denied host shell, other-project data,
audit/refusal state, benchmark internals, or a tool outside the role.

Evidence: exact role fixtures and evaluation in `corpus-core/tests/roles.rs`,
plus store tests `red_lines_survive_scalar_permissions`,
`a_stored_bash_allow_cannot_survive_a_restricted_role`, and
`rendered_permissions_deny_other_projects_absolutely`.

### SEC-AUTH-3: host administration and scoped management stay distinct

Project creation/deletion, cross-project copying, and other-project access are
operator-only. Curator and Super receive only the scoped subset; Super may wipe
its own corpus with confirmation but is not a host-global operator. The admin
artifact cannot depend on core, plugin, launch, research MCP, app, or Goose.

Enforcement: typed registry grant sets, scoped schemas, separate MCP binaries,
and `scripts/check-dependency-policy`.

Evidence: `admin_profile.rs::host_admin_tools_are_absent_from_the_research_catalog`,
`curator.rs::super_can_manage_and_wipe_only_its_scoped_project`, and the exact
workspace/admin dependency policy.

### SEC-AUTH-4: destructive authority is bound to fresh intent

Destructive model-facing tools first return a dry-run preview. Commit requires
a short-lived, single-use token bound to operation and target. Mismatch,
expiry, and even a failed mutation consume the token. Directory deletion also
requires explicit recursive intent.

Evidence: `corpus-admin::confirmation` tests and the destructive contracts in
`corpus-mcp/tests/admin_profile.rs` and `curator.rs`.

## Filesystem and persistence invariants

### SEC-FS-1: data, resources, and run namespaces are separate

Operator data lives below `CORPUS_HOME`/`CORPUS_STORE`; shipped resources are
read-only and independently resolved. Project run directories live outside the
repository, expose exactly one project, and reject symlinked boundary
components or an unexpected second project. Each pin-keyed launch view exposes
only the exact source IDs and commit SHAs in that launch plan; the shared cache
is never linked wholesale into an agent cwd. Durable mission records retain a
validated, relocatable source-view id; session control and accounting rebuild
the path beneath that mission's project root rather than trusting a stored
absolute path or falling back to the project staging root.

Evidence: store path tests, CLI resource-root binary test, and
`run_workspace.rs`, including
`a_run_source_view_exposes_only_exact_pins_and_refuses_nested_symlinks` and
`persisted_workspace_ids_resolve_only_beneath_the_project_views_root`.

### SEC-FS-2: model-chosen paths cannot escape their resource root

All entry paths are validated relative paths under allowed corpus categories.
Absolute paths, traversal, bare categories, immutable `runs`, symlinks, and
cross-device/target collisions fail before publication. Agent and project
clone/copy, source-view provisioning, and plugin installation preflight entire
source trees and publish no partial destination on refusal.

Evidence: `corpus-store/tests/curation.rs`, finding transactional/symlink
coverage, agent and project clone symlink/cancellation tests, pinned-source
view tests, and plugin installation entrypoint/symlink tests.

### SEC-FS-3: durable replacement is complete or preserves prior state

Writers use unique same-directory staging and atomic replacement or exclusive
creation. Concurrent writers do not share staging paths; a failed replacement
cleans staging and leaves the old target intact.

Evidence: all tests in `corpus-store/src/filesystem.rs` and collision-safe
finding writer tests.

### SEC-FS-4: curated plugin archives fail closed before installation

Normal plugin installation resolves only Corpus's compiled catalog, requires
HTTPS and a pinned SHA-256 digest, bounds compressed size, expanded size, and
entry count, and accepts only normal relative paths, directories, and regular
files. The extracted manifest id and version must match the catalog before the
immutable local installer runs. Local unpacked bundles remain an explicitly
named development path and still pass the existing manifest/tree checks.

Evidence: `corpus-core::plugin_distribution::tests`, including checksum,
catalog metadata, manifest identity, archive path, and extraction tests; plus
`corpus-core/tests/plugin_installation.rs` for immutable publication checks.

### SEC-DUR-1: cleanup identity survives failure and restart

Environment session identity is durable outside project subtrees. Active or
failed cleanup state blocks deletion and relaunch until teardown succeeds.
Stopping attempts every cleanup step and retains enough session/transcript
identity for retry; restart can recover detached sessions.

Evidence: mission deletion/environment tests,
`detached_stop_preserves_identity_when_cleanup_fails_then_allows_retry`,
`failed_environment_survives_restart_and_blocks_relaunch_and_delete`, and
`restarted_app_recovers_a_durable_detached_session`.

### SEC-DUR-2: lifecycle reconciliation rejects stale work

Background results carry project generation, corpus revision, and optional run
identity. Navigation, deletion, or a newer generation makes stale results
inapplicable. Acknowledgement whose durable identity changed remains visible
and retryable rather than reporting success.

Evidence: `job_scope_guard_rejects_navigation_and_generation_staleness`,
corpus revision tests, and
`acknowledged_delivery_with_stale_persistence_is_reported_retryable`.

## Process and protocol invariants

### SEC-PROC-1: child processes are bounded and owned

Corpus resolves executables through explicit precedence, captures stdout and
stderr with finite limits, assigns owned process groups, enforces deadlines,
and kills/reaps on timeout. Tmux operations use exact Corpus session names and
separate dynamic values into argv or a generated owner script.

Evidence: `bounded_output_caps_stdout_and_keeps_stderr_separate`,
`timeout_kills_and_reaps_the_owned_process_group`, shell-rendering tests, and
tmux argv/session tests.

### SEC-PROC-2: plugin replies cannot redefine the selected protocol

An installed immutable manifest declares protocol and capabilities. V1 hello
must agree before lifecycle or tool calls proceed. Reply IDs, terminal result
shape, raw frame and queued-stream size, lifecycle progress count, oracle
catalog/log size, and call duration are bounded before JSON deserialization.
Timeout or protocol loss retains reconciliation identity so work is not
silently repeated.

Evidence: `corpus-core::plugin::framing_tests`, plus
`corpus-core/tests/protocol.rs`, including capability drift, mismatched reply,
deadline, and cancellation tests; MCP V1 session tests prove durable session
and project matching.

### SEC-PROC-3: local session control is loopback and identity-bound

OpenCode HTTP control accepts only explicit loopback URLs with ports. Delivery
and acknowledgement require the exact bound session, pin-specific workspace,
and message/turn evidence; Corpus does not infer success from quiet output.

Evidence: `http_adapter_refuses_non_loopback_and_implicit_ports`,
`turn_start_evidence_is_durable_and_scoped_after_the_exact_launch`, and
`delivery_terminal_is_scoped_to_the_exact_legacy_user_message`, plus app
workspace-isolation, legacy-recovery, dispatch, and live-cost tests.

## Audit, diagnostics, and test isolation

### SEC-AUDIT-1: privileged scoped mutations are accountable

Curator/Super mutation intent and outcome are appended outside the writable
project tree. If the audit intent cannot be recorded, the mutation is refused.
Reads remain unaudited. Refusal logging is best-effort and never changes the
tool outcome; unresolved projects land in a sanitized `_unscoped` record.

Evidence: curator audit tests, `an_unrecordable_act_is_refused`, and store audit
and refusal tests.

### SEC-TEST-1: Qwen3.8 MLX integration is globally serial

Every real-model test accepts only a `qwen3.8` MLX identity and acquires one
cross-process OS lease before inference. The dedicated integration preflight
discovers and reports the installed digest, and the curator system campaign
persists that identity with its evidence. The lease remains held through
cleanup and artifact capture. `--test-threads=1` is required but is only a
secondary guard.

Evidence: `corpus-integration::model_lock`, the non-MLX refusal test, the live
model smoke, and the full curator system campaign.

## Priority abuse cases

1. A curator supplies another project or forged parent origin. The scoped MCP
   removes project from its schema, injects proven scope, and derives origin
   from the launcher identity pair.
2. Two curators use identical child mission slugs. Completion groups by exact
   parent and child run identities, never slug or UI selection alone.
3. A stale completion acknowledges a newer run. The compare-and-advance fails,
   remains retryable, and emits a persistence-failure delivery event.
4. A model proposes traversal, a symlink, or a destructive call without fresh
   confirmation. Validation below the schema refuses before publication.
5. A plugin stalls, floods output, changes capability claims, or returns the
   wrong ID. The call fails within bounds and the owned process is cleaned up.
6. Two Cargo invocations start live model tests concurrently. Only one can hold
   the global lease; the other fails before entering the model boundary.

## Security review gate

Changes to chat, prompts, tools, MCP, role policy, state, store, launch,
plugins, source handling, permissions, transcripts, or curator delivery must:

1. name the affected invariant IDs in review;
2. add characterization evidence before changing unclear behavior;
3. run strict Clippy and the full hermetic workspace tests;
4. run the serial Qwen3.8 MLX suite when model/tool/launch behavior changes;
5. retain first-attempt artifacts for every live failure;
6. update this document if authority, trust assumptions, or failure policy
   changes.

Accepted dependency risks are not duplicated here. They require dated,
package-specific rationale and removal triggers in
[`supply-chain-policy.md`](supply-chain-policy.md).
