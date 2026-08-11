# corpus

A local-first platform for autonomous vulnerability research: a team of AI
researchers that investigate codebases at pinned commits, hunt for novel
vulnerabilities in a sandboxed environment, and compile everything they
learn into a private, verifiable knowledge base — the corpus.

The core is the research system. Everything project-specific (how to boot a
regtest network, what invariants must hold, which tools an attacker gets)
is a plugin, so the community can point corpus at any open-source project
with a reproducible test environment.

## Why this exists

Unit tests are common. Integration tests are less common. Adversarial
testing with executable verification barely exists — and the AI tools
emerging for it (XBOW, Strix, Buttercup, CAI) are cloud-first,
web-app-focused, or require paid frontier APIs. Two problems with that:

1. **Privacy.** A verified 0-day in your own infrastructure is not
   something you want to stream through a third-party API or a hosted
   dashboard. Even Google's oss-fuzz-gen keeps its reports private
   precisely because they contain undisclosed vulnerabilities.
2. **Ground truth.** Most agentic security tools produce *claims*. For
   bitcoin/ecash infrastructure we can do better: regtest environments
   (bitcoind + Lightning nodes + the real service under test) let every
   claimed exploit be *executed and checked against invariants* — value
   conservation, auth enforcement, state-machine legality. No oracle trip,
   no finding.

corpus runs entirely on your own hardware: local open-weight models
(via ollama or any OpenAI-compatible server), local sandboxed execution,
a local encrypted git repo as the knowledge store. Frontier APIs are an
opt-in escape hatch, never a requirement.

## Design pillars

- **Verification-first.** A finding exists only when backed by an
  executable PoC that trips a programmatic oracle. This is what makes
  local, weaker-than-frontier models viable: generation becomes search.
- **Pinned heads.** Every run records the exact commit under test.
  Findings are reproducible; replaying a PoC against a later commit is how
  fixes get verified and regressions get caught.
- **Meticulous, tamper-evident logging.** Every model call, tool call,
  and environment state is recorded in an append-only, hash-chained run
  log; the corpus lives in a signed, optionally encrypted git repository.
- **Trust domains.** Attack code executes in an egress-denied sandbox.
  Internet research happens in a separate zone with no code execution.
  The model endpoint is host-side. Plugins are host-trusted by nature.
- **Two jobs.** *Campaigns* (ad hoc, scoped by mission or diff) and
  *watch mode* (continuous: new commits replay findings, spec changes and
  external advisories generate new missions).
- **A lab for open-weight models.** corpus benchmarks models against
  *your* purpose — funded Lightning attacks, quote state-machine abuse,
  race conditions — not generic leaderboards. Every finding and run is
  tagged with the exact model that produced it; when a new open-weight
  model drops, the forensic suite tells you whether it got better or
  worse at the work you actually care about.

## Status

Pre-alpha. The reference harness is the first-party `cdk-regtest`
environment plugin in this repo — absorbed from the vul-lab proof of
concept (2026-08-10) so corpus is self-contained: a sandboxed agent, an
oracle suite, a regtest Lightning faucet, and no external harness
dependency.

In this repo: Rust workspace (`crates/corpus-core`, `crates/corpus-mcp`
MCP server, `crates/corpus-cli` with the ratatui TUI), the plugin
protocol, the reference environment plugin (`plugins/cdk-regtest`),
and the model registry (`benchmarks/models.yaml`).

(Design context — architecture, research landscape, decisions, and the
live roadmap — lives in `dev/`, which is machine-local scratch and not
included in this repository.)

## Authorized use

corpus is an offensive-capable defensive tool. Point it only at systems
you own or have explicit written permission to test. The default target is
always a local, disposable test environment — never production.
