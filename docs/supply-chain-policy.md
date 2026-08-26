# Supply-chain Policy

Date reviewed: 2026-08-26

Corpus uses `cargo-deny 0.20.2` to make dependency provenance and review
decisions executable. The policy covers the complete all-feature graph for the
supported Linux release target and the Apple Silicon development/release
target. It complements, rather than replaces, the package-boundary check in
`scripts/check-dependency-policy`.

Run both local gates with:

```sh
./scripts/check-dependency-policy
./scripts/check-supply-chain
```

Install the pinned policy tool with:

```sh
cargo install --locked cargo-deny --version 0.20.2
```

CI uses the matching `EmbarkStudios/cargo-deny-action` release and always
checks the committed lockfile. A policy-tool upgrade is a reviewed dependency
change: update the script, CI action, this document, and `deny.toml` together.

## Enforced rules

- RustSec vulnerabilities fail the gate. Yanked packages fail, unsound
  advisories apply to the full graph, and unmaintained advisories apply to
  workspace packages. An offline advisory database may be at most 30 days old.
- Every accepted SPDX license is listed in `deny.toml`. Font licenses are
  allowed only for the exact `epaint_default_fonts 0.31.1` package rather than
  becoming workspace-wide allowances.
- Crates may come only from crates.io, the exact Goose Git repository, or
  workspace/local paths. Every Git dependency must use a full revision.
- Multiple versions fail unless an exact version has a documented reason in
  `bans.skip`. A new version split therefore requires an explicit review; the
  policy does not blanket-ignore a dependency subtree.
- The wildcard-requirement lint is disabled because cargo-deny 0.20.2 cannot
  resolve the optional workspace-inherited Goose Git dependency during that
  lint. Registry and Git provenance, the Goose full revision, and the locked
  graph remain independently enforced.

## Reviewed advisory exceptions

Two RustSec advisories currently share one constrained dependency path:

| Advisory | Locked package | Reachability and decision | Removal trigger |
|---|---|---|---|
| `RUSTSEC-2026-0194` | `quick-xml 0.30.0` | Quadratic duplicate-attribute checking is reachable only through Egui's Linux accessibility `zbus_xml 4` line. Corpus is a desktop client rather than a network XML service, but a hostile local bus peer may still pose an availability risk. | Upgrade the Egui/accesskit/zbus line to one accepting `quick-xml >=0.41`, or remove that dependency path. |
| `RUSTSEC-2026-0195` | `quick-xml 0.30.0` | Unbounded namespace allocation has the same Linux accessibility path and local availability risk. There is no compatible patched release in the `zbus_xml 4` requirement. | Same as above. |

These exceptions were reviewed on 2026-08-26 and must be reviewed again by
2026-11-26 if upstream convergence has not removed them. They are listed by
advisory identifier, never by package subtree. `RUSTSEC-2026-0258` was found
during policy introduction and remediated immediately by updating `h2` from
0.4.15 to 0.4.16; it is not ignored.

## Duplicate-version review

The exact duplicate versions and their reasons live beside the enforcement in
`deny.toml`. The current groups are:

- Goose versus Corpus protocol, network, crypto, image, and parser lines;
- Egui terminal, clipboard, accessibility, Wayland, and macOS platform lines;
- transitive macro, serialization, randomness, and collection generations.

When a lockfile update removes a split, cargo-deny reports the now-unused skip
entry and the exception must be deleted. When it introduces a split, prefer
unification first. Add an exact skip only when upstream version constraints
make convergence impossible, and record the owning dependency and removal
condition in its reason.

## Goose revision or crate migration gate

Goose remains intentionally untouched until its planned crate is available.
Every future revision or crate-version change must be handled as a dedicated
event:

1. record the intended full Git revision or exact crate version and update the
   lockfile without unrelated package churn;
2. inspect all source, license, advisory, and duplicate-version changes and
   update narrowly scoped exceptions only with written risk/removal rationale;
3. run both supply-chain gates, strict Clippy, the full hermetic workspace, and
   both default and no-default-feature application configurations;
4. run the embedded-management and full curator orchestration campaigns from
   `docs/testing.md` using only `qwen3.8:27b-mlx`, under the global model lease
   and with `--test-threads=1`;
5. retain the first-attempt logs and correlated run artifacts with the review.

No advisory, source, or duplicate exception should be widened merely to make a
Goose update pass.
