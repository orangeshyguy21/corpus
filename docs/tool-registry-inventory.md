# Tool registry inventory

Recorded 2026-08-25 at the start of Phase 5.

## Current surfaces

| Surface | Tools | Catalog/schema owner | Dispatch/argument owner | Authorization owner |
|---|---:|---|---|---|
| Host administration | 35 | `corpus-admin/src/lib.rs::catalog` | `corpus-admin/src/lib.rs::dispatch_with_origin` and individual handlers | Dedicated host artifact plus destructive confirmation gate |
| Scoped project management | up to 29 by role | Filtered host catalog via `scoped_catalog` | `corpus-mcp/src/tools.rs::scoped_management_dispatch` then corpus-admin | `AgentRole::admin_tools`, proven project injection, Curator ceiling |
| Research/environment | 10 | `corpus-mcp/src/tools.rs::catalog` | `corpus-mcp/src/tools.rs::dispatch_inner` and individual handlers | `AgentRole::allows`, scope, probe, oracle, and budget gates |
| Management chat | 35 | Host catalog exposed through Goose extension configuration | Host admin MCP | `chat/team.rs` specialist lists plus approval classification |

The host tools are grouped as projects (5), agents (12), missions (9), corpus
and findings (8), and model discovery (1). The research tools are
`target_info`, `sandbox_exec`, `sandbox_write`, `oracle_list`, `oracle_run`,
`faucet`, `wallet_fund`, `finding_write`, `attack_save`, and
`technique_save`.

## Metadata currently maintained separately

| Concern | Current source |
|---|---|
| Advertised name, description, input schema | Handwritten JSON catalogs in corpus-admin and corpus-mcp |
| Handler routing | String matches in both dispatchers |
| Agent role ceiling | `corpus-store/src/agents/roles.rs` tool arrays |
| Read/write/destructive classification | `corpus-app/src/chat/team.rs` and `corpus-mcp/src/tools.rs` |
| Confirmation policy | corpus-admin destructive list and chat approval classification |
| Audit target/category | corpus-mcp scoped-management helpers |
| UI invalidation | `chat/team.rs::mutated_area` |
| Scoped project removal/injection | corpus-admin `scoped_catalog` and corpus-mcp `with_project` |

Existing completeness tests catch several name-set mismatches, but they cannot
prove that a handwritten schema matches what a handler actually accepts. A
field can also acquire different classification, audit, authorization, and UI
refresh behavior because those tables do not share one definition.

## Phase 5 migration order

1. Establish one typed Serde input and generated `schemars` schema path on a
   low-risk read-only tool.
2. Add declarative definition metadata without moving authorization gates.
3. Migrate one domain at a time, keeping catalog and dispatch completeness
   tests green after every tool.
4. Derive chat classification/refresh and scoped-management audit metadata
   only after the registry carries equivalent data.
5. Split handlers by project, agent, mission, corpus, model, environment,
   oracle, and finding domains.

The first boundary is `model_list`: its `ModelListArgs` is now the single
source for both deserialization and the advertised input schema. Unknown
fields remain accepted for forward compatibility, defaults remain unchanged,
and a present field with the wrong type is now rejected explicitly instead of
being silently treated as absent. `schemars` 1.2.2 was already in the lockfile
through the existing dependency graph; making it direct adds no locked
package.
