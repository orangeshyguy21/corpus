# Decisions — completed plan closeouts

One paragraph per completed plan: what was decided, why, where the code
lives. Plans in `dev/` that reach DONE are collapsed here and deleted,
per the plan-hygiene rule in `AGENTS.md`. Dated entries, oldest first;
the roadmap (`dev/roadmap-plan.md`) stays the live state-of-the-world.

## 2026-08-10 — Absorb vul-lab into corpus as the `cdk-regtest` plugin

Decided to **absorb** (not wrap) the vul-lab proof-of-concept harness from
`~/Sites/cdk/vul-lab` into the corpus repo as the first-party environment
plugin `plugins/cdk-regtest`, so corpus is fully self-contained. Why: the
plugin protocol needs a real reference environment, and the harness load —
~750 lines of bash + docker for arena, sandbox, oracles, faucet — had
already proved itself in alpha-1; keeping it external would force
double-maintenance and an external repo dependency. The arena lives in
`plugins/cdk-regtest/arena.sh` (networks/gateway/sandbox/agent image,
renamed `vul-lab-*` → `corpus-*`), oracles under
`plugins/cdk-regtest/oracles/`, the faucet in `faucet.sh`, the protocol
adapter in `plugins/cdk-regtest/plugin` (JSONL over stdio). Protocol
extensions landed in `crates/corpus-core/src/plugin.rs`:
`sandbox_exec` (plugin owns the long-lived sandbox), `faucet`, `tools`;
`call_oracle` returns `{verdict, log}`. `corpus-mcp` now drives everything
through the plugin (`CORPUS_PLUGIN_DIR`), the agent tool catalog unchanged.
The old vul-lab directory is tombstoned but not deleted (see the cdk repo's
`vul-lab/TOMBSTONE.md`).

- Code: `plugins/cdk-regtest/`, `crates/corpus-core/src/plugin.rs`,
  `crates/corpus-mcp/src/`.
- Verification: protocol unit tests against a fake echo plugin; full MCP
  smoke (probe → sandbox → faucet → wallet_fund → oracles → finding gate)
  passing on the plugin's own arena after the old arena was torn down.

## 2026-08-10 — Pinned source access for the operator (right corpus in, wrong corpus out)

Decided to give the sandbox-role agent the **right** research corpus and
cut off the **wrong** one. Why: black-box-only attackers burn steps probing
endpoints the source would answer in one grep, and — worse — one alpha-era
attacker, lacking a sanctioned source, read `benchmarks/` and `plugins/`
(i.e. the answer key) from the host, contaminating the run. Design: a
git-ignored `sources/` cache of pinned upstream trees
(`sources.toml` maps repo → tag → commit SHA; trees are mounted read-only
into the sandbox at `/opt/src/cdk` and `/opt/src/nuts`), plus hardened
permissions (host reads fully denied to the operator; the benchmark YAML
and plugin internals are never mounted). A run whose transcript shows reads
of `benchmarks/**` or `plugins/**` is contaminated and unscored. Probe
gains a version↔sha consistency check so a mission can't run against a mint
that doesn't match the mounted source.

- Code: `sources.toml`, `plugins/cdk-regtest/setup.sh` (fetch + verify),
  `plugins/cdk-regtest/config.toml` `[sources]`, `arena.sh`
  `source_mount_args`, `.opencode/agent/operator.md` (all host reads
  denied).
- Evidence: alpha2 (no source) 17 execs/abandoned → alpha3 (source in
  context) 5 execs/completed.

## 2026-08-10 — Research team reorg: operator + researcher roles

Decided on a **two-role v1 team** — `operator` (sandbox role: takes a
hypothesis, iterates a PoC with oracle feedback, documents its own work)
and `researcher` (research-zone role: reads internet + store + pinned
source, executes nothing, produces hypotheses and curates cards) — folding
the earlier scout and librarian duties into the researcher and renaming
`attacker` → `operator` per the architecture's role model. Why: one
oversized all-rounder agent underperforms versus specialized roles, the
alpha-1 attack/scribe split proved the shape, and the trust boundary is
best enforced in opencode permissions, not prompts. The researcher holds
`corpus_sandbox_exec`/`faucet`/`wallet_fund`/`oracle_run`/`finding_write`/
`attack_save` denied and `read` denied on `benchmarks/**`; the operator has
all host reads denied. A new `technique_save` MCP tool (no oracle gate, but
`run_log` must cite an existing `store/runs/` file) makes negative results
first-class corpus value.

- Code: `.opencode/agent/operator.md`, `.opencode/agent/researcher.md`,
  `crates/corpus-mcp/` (the `technique_save` tool), `crates/corpus-cli/`
  (`corpus run --research` renames the `--scribe` pass), `store/hypotheses/`.
- Docs: `docs/architecture.md` "The research team"; `docs/alpha-1.md`
  carries role-rename pointers.