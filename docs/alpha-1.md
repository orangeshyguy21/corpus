# Alpha 1 — results

Date: 2026-08-10. Status: alpha milestone reached (harness + loop proven
end-to-end). This is the honest record: what works, what the evidence is,
what still needs proving. Post-alpha, the harness was absorbed into corpus
as the first-party plugin (see "vul-lab absorbed" below).

## What was built

- `crates/corpus-mcp` — the agent tool surface (7 tools):
  `sandbox_exec`, `target_info`, `oracle_run`, `faucet`, `wallet_fund`,
  `finding_write`, `attack_save`.
- `crates/corpus-core` + `crates/corpus-cli` — protocol stubs (negotiate
  with host transforms) and CLI plumbing.
- `.opencode/agent/attacker.md`, `.opencode/agent/scribe.md` — the first
  two researchers. Attacker has sandbox network; scribe has store-write
  but no sandbox. Trust domains enforced via opencode permissions.
(Renamed 2026-08-10 → **operator** and **researcher**; see
   `docs/decisions.md`.)
- `store/` — findings/, attacks/, techniques/, runs/ (git-ignored unless
  committed deliberately).

## Proven end-to-end (all tool calls exercised against a live regtest CDK mint)

Every one of the 7 tools was called successfully over the MCP protocol,
and separately by the attacker agent in live runs:

1. **Funding through the faucet works**, but only after fixing the real
   bug: `mint-pending` checks pending *proofs*, not pending *quotes*.
   The claim path is `cdk-cli mint <url> -q <quote_id>`. `wallet_fund`
   now does quote → pay → claim in one deterministic call (evidence:
   funded 2100 sats, issued "Minted 2100 sat", balance confirmed).
2. **oracle_run executes the host-side invariant** (020-conservation
   returned a live verdict with real mint liabilities).
3. **finding_write is gated** — recorded a smoke-test finding, correctly
   marked unverified.
4. **attack_save writes attack.md + run.sh** artifacts into store/.

## The skill-feedback loop — quantified

The attacker was ran twice against the same mission (concurrent token
redemption race against the target mint):

| run | technique card in context | tool calls | outcome |
|-----|---------------------------|------------|---------|
| alpha2 | no | 17 sandbox_exec + 1 wallet_fund | ran out of budget mid-attack; no oracle, no save |
| alpha3 | yes (its own prior notes) | 5 sandbox_exec + 1 of each tool | completed: race → oracle → attack_save; honest conclusion |

The loop works: hostile experience → scribe distills it into a technique
card → the card cuts the next run's friction by ~4x and gets the full
chain completed. This is the core value proposition of the project,
demonstrated rather than asserted.

## First research result (negative, honest)

**concurrent-receive-race: not vulnerable.** Ten parallel `receive`
attempts of one identical token into independent wallets; exactly one
(any winner) claimed the proofs; nine got Token-Pending/Already-Spent;
oracle 020-conservation held. The mint serializes swaps correctly.

## Known gaps going into alpha 2

- **Budget truncation**: local 35B model still runs long missions to
  exhaustion; missions must include the proven technique cards (skills)
  to stay inside budget, or use a stronger model.
- **No critic role yet**: evidence quality is human-reviewed only.
- **Scribe transcript fidelity**: it worked on a compact transcript;
  needs a test on a full-size one.
- **No TUI**; store review is via git / files.
- **No forensic benchmark**: no historical bug is re-findable yet. This
  is the acceptance test for alpha 2.

## vul-lab absorbed (same day)

The working vul-lab harness in the cdk repo was absorbed into corpus as
the first-party `plugins/cdk-regtest` plugin — corpus no longer depends on
`~/Sites/cdk/vul-lab`. Details in `docs/decisions.md`; summary:

- Arena (networks, gateway, sandbox, agent image) ported to
  `plugins/cdk-regtest/arena.sh`, names renamed `vul-lab-*` →
  `corpus-*` (`corpus-arena`, `corpus-arena-egress`, `corpus-target-gw`,
  `corpus-sandbox-testing`, image `corpus-agent:local`). The old arena and
  the new plugin coexist without collision, which is how verification ran.
- Oracles and the faucet moved under the plugin; `VUL_LAB_*` env vars
  renamed `CORPUS_*`. The `/tmp/cdk_regtest_env` contract stays — it
  belongs to the regtest environment the plugin targets.
- Protocol extended in `corpus-core`: `sandbox_exec` (plugin owns the
  long-lived sandbox), `faucet`, `tools`; `call_oracle` returns
  `{verdict, log}` so finding bodies can cite the evidence.
- `corpus-mcp` now drives everything through the plugin protocol
  (`CORPUS_PLUGIN_DIR` instead of `CORPUS_VUL_LAB`); the tool catalog is
  byte-identical, so agents see no change. Session budget stays corpus-side;
  per-payment cap and regtest-only enforcement stay plugin-side.
- Verification: 7 protocol unit tests against a fake echo plugin; full
  smoke (probe → sandbox → faucet → wallet_fund 2100 sats → oracles →
  finding gate) run after `vul-lab down` tore down the old arena — the
  egress-deny isolation probe, the conservation oracle, and
  `wallet_fund` all still pass on the plugin's own arena.