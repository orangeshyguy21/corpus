# Phase 6 Inventory: Views, Observability, and Dependencies

Date: 2026-08-25

This inventory fixes the measured Phase 6 starting point. File size identifies
where to inspect; an extraction still needs a responsibility boundary and
behavioral coverage before it lands.

## UI hotspots

The largest application UI owners at Phase 6 entry are:

| File | Lines | Main responsibilities visible at entry |
|---|---:|---|
| `views/projects.rs` | 1,821 | project dashboard, finding summary, plugin state/operations, corpus visualization, run logs, and cost reporting |
| `views/agents.rs` | 1,351 | agent drafts/forms, role and policy editing, subagents, capability preview, and structure diagram |
| `sidebar.rs` | 1,281 | project/agent/mission navigation, row/menu mechanics, lifecycle actions, and compact corpus status |
| `chat/panel.rs` | 1,223 | transcript rendering, tool/permission cards, input/activity controls, model picker, and usage projection |

The first view extraction should follow an already-testable responsibility,
not simply move a contiguous number of lines. `views/projects.rs` is the
leading candidate because finding summary, plugin configuration, corpus/run
projection, and cost reporting are separable domains with existing pure tests.

## Observability baseline

Production code has no structured tracing dependency or subscriber. Outside
ignored live-probe diagnostics, the inspected app/core/store/admin paths expose
only two cleanup failures through `eprintln!`, both in the launch subsystem.
State jobs and notices make failures visible in the UI, but they do not provide
correlatable project/mission/run spans for operator diagnosis.

The first observability work should define stable event fields and ownership
before adding a subscriber: project, mission, run/session identity, operation,
generation, elapsed time, outcome, and retryability. Lifecycle and delivery
coordinators are the highest-value seams; UI paint functions should not emit
operational events.

## Dependency baseline

### Direct image decoder

At entry, `corpus-app` depended directly on `image 0.25` with default features
for one call that decoded the embedded PNG application icon. Default feature
unification enabled unrelated format decoders and Rayon. Eframe 0.31.1 already
provides `eframe::icon_data::from_png_bytes` for exactly this conversion and
already owns the required transitive PNG support.

The first Phase 6 slice removes the direct app/workspace `image` dependency,
uses eframe's icon adapter, and locks the bundled 250×250 RGBA result with a
test. The `image` crates remain transitively present through eframe/egui and the
untouched Goose dependency; this slice removes Corpus's redundant direct edge
and its default-feature request rather than claiming the packages disappear.

### Syntect backend

The sixth Phase 6 slice switched `corpus-app`'s Syntect 5.3.0 configuration
from `regex-onig` to `regex-fancy` after expanding the bundled Markdown/JSON
source-preservation and palette fixtures. The resolved graph contains neither
`onig` nor `onig_sys`; Syntect adds its independently constrained pure-Rust
`fancy-regex 0.16.2`. Goose's 0.17/0.19 versions remain untouched. The cold
focused fixtures move from about 0.04 seconds under Oniguruma to 0.69 seconds
under pure Rust in an unoptimized process. After lazy initialization, a
temporary probe measured 100 distinct uncached edits at about 124 milliseconds
total, acceptable for the app's small cached editable fields.

### YAML

The seventh Phase 6 slice centralized all production YAML operations behind
`corpus_store::yaml` and documented the persisted-surface and replacement gate
in [`yaml-compatibility.md`](yaml-compatibility.md). Direct `serde_yaml` edges
were removed from observe, core's tests, and integration; the integration edge
was unused. Corpus now has one direct deprecated-backend owner in store, while
Goose's transitive use remains untouched. Adapter and production registry tests
lock scalar types, nested maps, unknown fields, and actionable source
locations. The eighth slice then passed the byte-exact A/B campaign and retained
`yaml_serde 0.10.7`. It adds maintained `libyaml-rs 0.3.0`, an explicitly unsafe
C2Rust translation of libyaml; this is maintained custody rather than a
memory-safety claim. Deprecated `serde_yaml` remains only through untouched
Goose. `serde-saphyr` stays the larger pure-Rust/budgeted hardening alternative.

### Goose

Goose remains untouched. Its source dependency, image 0.24 line, and ICU/RMCP
type-identity pins stay in place until the planned Goose crate migration.

### Supply chain

The ninth Phase 6 slice added the executable [`deny.toml`](../deny.toml) policy
and pinned `cargo-deny 0.20.2` in both the local gate and CI. Unknown registries
and Git repositories are denied, Git sources require full revisions, licenses
are allowlisted, and duplicate versions require exact reviewed exceptions.
Goose remains the sole allowed Git repository and is still pinned to its
existing revision.

The initial advisory pass found and upgraded vulnerable `h2 0.4.15` to 0.4.16.
Two `quick-xml 0.30.0` denial-of-service advisories remain narrowly ignored on
Egui's Linux accessibility `zbus_xml 4` path because that version requirement
cannot accept the fixed 0.41 line. Their reachability, local availability risk,
review date, and removal trigger are recorded in
[`supply-chain-policy.md`](supply-chain-policy.md).

## Slice order

1. Remove the redundant direct image decoder edge.
2. Extract the first responsibility owner from `views/projects.rs` behind its
   existing projection tests.
3. Define and instrument stable lifecycle/delivery tracing fields.
4. Switch Syntect to its non-Oniguruma backend after syntax fixtures and build
   measurements. Completed in the sixth Phase 6 slice.
5. Design the YAML compatibility campaign before selecting a replacement.
   Completed in the seventh Phase 6 slice.
6. Trial the maintained YAML fork behind the compatibility adapter. Completed
   in the eighth Phase 6 slice.
7. Enforce advisories, licenses, sources, and reviewed duplicates. Completed in
   the ninth Phase 6 slice.
