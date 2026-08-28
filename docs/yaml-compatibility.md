# YAML compatibility and replacement gate

Status: compatibility gate passed; Corpus uses `yaml_serde 0.10.7`.

## Why this is a separate campaign

Corpus persists YAML in operator-owned state and embeds it as frontmatter in
model-authored Markdown. A parser change can silently alter scalar types,
enum tags, unknown-field behavior, mapping order, or error locations even when
the code compiles. The current `serde_yaml 0.9.34+deprecated` repository is
archived and explicitly names no official successor, so replacement is a data
migration decision rather than a package rename.

All production parsing and serialization now enters through
`corpus_store::yaml`. That module owns `from_str`, `from_value`, `to_string`,
`to_value`, the mapping/value representation, and a backend-neutral error with
one-based location data. `corpus-observe` consumes that boundary for the
shipped model registry. `corpus-core` tests and `corpus-integration` no longer
declare their own YAML dependencies.

## Persisted and shipped surfaces

| Surface | Trust and mutation behavior | Compatibility requirement |
| --- | --- | --- |
| `project.yaml` | Durable, operator-editable, rewritten by Corpus | Required fields and defaults retain meaning; unknown fields remain readable and may be dropped on a typed rewrite, matching current behavior. |
| `app.yaml` | Durable preferences, fail-open | Missing, unreadable, and malformed input still yields defaults; successful writes remain atomic. |
| `agents/*/agent.yaml` | Durable authority sidecar, fail-closed | Missing or malformed data must never invent a role; enums, provenance, deletion intent, and subagent-role maps round-trip. |
| `missions/*.md` | Durable YAML frontmatter plus byte-preserved Markdown body | Legacy scalar launch requests and current structured requests both parse; lifecycle, dispatch, deletion state, and optional OpenCode workspace identity round-trip; records written before the workspace field default it to absent and remain readable; body bytes are not reformatted. |
| Finding Markdown | Model-authored frontmatter plus evidence body | Extension metadata, quoted ambiguous scalars, booleans, integers, nested source pins, and severity/sensitivity fields keep their types; malformed cards remain warnings rather than widening trust. |
| Rendered agent Markdown | Generated security policy frontmatter | Canonical ordering, deny rules, glob keys, YAML quoting, and role fixtures remain byte-exact. |
| `benchmarks/models.yaml` | Shipped, read-only registry | Absent file remains an empty registry; typed numeric/list/string fields and unknown fields parse; malformed input reports an actionable source location. |

The integration scenario YAML is currently copied into failure evidence but is
not parsed at runtime, so its former direct dependency declaration was dead and
has been removed.

## Locked compatibility probes

The adapter tests fix the backend-neutral contract for:

- strings that resemble booleans, nulls, numbers, comments, or mappings;
- Unicode, real booleans, integers, and nested ordered maps;
- accepted unknown fields and their current typed-reserialization behavior;
- one-based line and column diagnostics for malformed input.

Existing project, preference, agent rendering/repository, mission legacy and
current lifecycle, frontmatter, finding contract, role fixture, and hermetic
curator campaigns remain part of the replacement gate. Model-registry tests now
add typed scalars, unknown fields, and malformed-location coverage through the
same production loader.

## Candidate trial order

1. **`yaml_serde 0.10`** was selected after the compatibility trial. Its repository
   describes it as the YAML organization's actively maintained `serde_yaml`
   fork with a migration-compatible API. The current manifest is version
   0.10.7 and depends on `libyaml-rs 0.3`. This is the smallest behavioral
   experiment, but the resolved graph and unsafe/supply-chain posture still
   require review.
2. **`serde-saphyr`** is the hardening candidate. It is an independent,
   pure-Rust implementation with configurable structural/byte budgets and
   richer diagnostics. Those controls are attractive for model-authored and
   hand-edited YAML, but its schema-driven scalar behavior and representation
   API create more compatibility risk and must pass the same campaign.
3. **Saphyr's future Serde integration** is not selectable yet: the Saphyr
   repository describes `saphyr-serde` as forthcoming. Re-evaluate only after
   a stable release exists.

Primary research sources, checked 2026-08-26:

- [`serde_yaml` archival notice](https://github.com/dtolnay/serde-yaml/releases)
- [`yaml_serde` repository and migration guidance](https://github.com/yaml/yaml-serde)
- [`yaml_serde` current manifest](https://github.com/yaml/yaml-serde/blob/main/Cargo.toml)
- [`serde-saphyr` design, budgets, and serialization behavior](https://github.com/bourumir-wyngs/serde-saphyr/blob/master/README.md)
- [Saphyr project status](https://github.com/saphyr-rs/saphyr)

## Replacement acceptance gate

A candidate may replace the adapter backend only when:

1. all locked compatibility probes and exact role/frontmatter fixtures pass;
2. the full hermetic curator campaign and both app feature configurations pass;
3. malformed input remains bounded and produces file/actionable location data;
4. the dependency graph, licenses, unsafe code, advisories, maintenance, and
   duplicate versions are recorded by the supply-chain policy;
5. a before/after fixture diff shows no unexplained persisted-data changes;
6. Goose remains untouched and its independent transitive YAML version is not
   used to force Cargo type identity.

## Trial result

The archived backend's representative serialization was captured byte-for-byte
before the swap, including ambiguous string quoting, mapping order and nesting,
booleans, integers, and Unicode. Exact rendered-role fixtures and the finding
writer passed immediately before and after the swap. `yaml_serde 0.10.7` also
preserved unknown-field behavior and both malformed-input locations. The full
store, observe, downstream, hermetic curator, and two-feature app gates passed,
so Corpus retains the maintained fork.

The resolved graph adds `libyaml-rs 0.3.0`. Its own documentation describes it
as libyaml translated from C to unsafe Rust with C2Rust, and `yaml_serde` itself
contains narrow unsafe implementation sites. This is a maintenance and custody
improvement, not a pure-Rust memory-safety claim. `serde-saphyr` remains the
future option if bounded parsing and removal of this unsafe implementation
justify a deliberately broader behavior migration.

Deprecated `serde_yaml 0.9.34+deprecated` remains in the workspace only through
Goose. Goose and its dependency selection remain untouched.
