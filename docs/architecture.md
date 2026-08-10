# corpus architecture

Status: proposal, pre-alpha. The working harness from the vul-lab PoC is
absorbed into this repo as the `cdk-regtest` plugin (2026-08-10); read
docs/research.md for the competitive context.

## Overview

```
┌──────────────────────────── frontends ───────────────────────────┐
│  corpus-tui (ratatui, primary)     corpus-desktop (Tauri, later) │
└──────────────────────────────┬───────────────────────────────────┘
                               │ (same library API)
┌──────────────────────────────▼───────────────────────────────────┐
│  corpus-core (Rust lib + daemon)                                 │
│                                                                  │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────────┐  │
│  │ scheduler   │  │ agent runtime│  │ corpus store            │  │
│  │ campaigns + │  │ researchers, │  │ git repo: findings,     │  │
│  │ watch mode  │  │ roles, tools │  │ techniques, threat      │  │
│  └──────┬──────┘  └──────┬───────┘  │ models, runs, oracles   │  │
│         │                │          └─────────────────────────┘  │
│  ┌──────▼────────────────▼───────┐                               │
│  │ plugin host (subprocess,      │  model router (ollama /       │
│  │ JSONL over stdio)             │  OpenAI-compatible, local)    │
│  └──────────────┬────────────────┘                               │
└─────────────────┼────────────────────────────────────────────────┘
                  │ one plugin per target project
   ┌──────────────▼─────────────┐
   │ environment plugin (e.g.   │   host-trusted: boots envs, runs
   │ cdk-regtest, written in    │   oracles, provides tools/faucets
   │ any language)              │
   └──────────────┬─────────────┘
                  │ manages
   ┌──────────────▼─────────────┐
   │ arena: egress-denied       │   untrusted: agent code executes
   │ sandbox + real target      │   here; only off-bridge path leads
   │ (bitcoind, LN, mints)      │   to the targets
   └────────────────────────────┘
```

## Core entities

- **Target** — a project under test at a pinned commit (`repo`, `rev`,
  `plugin`). Pinning is what makes every finding reproducible.
- **Environment** — everything the plugin provides: lifecycle
  (up/down/reset/snapshot), targets (in-scope endpoints), oracles, tools,
  faucets.
- **Researcher** — an agent instance with a role, a mission, a model, and
  a step/time budget. Roles below.
- **Mission** — a scoped objective: markdown brief + constraints (allowed
  techniques, budget, target scope). Missions are files; they diff and
  review well.
- **Finding** — claim + executable PoC + verification status + provenance
  (commit, model, harness version, run id). Stored in the corpus.
- **Oracle** — executable invariant with the contract: exit 0 holds,
  exit 1 violated, exit 2 inconclusive. Host-side, outside the attacker's
  trust domain.
- **Run** — one execution of a mission: full hash-chained transcript,
  environment identity, model identity, verdicts.

## The research team (roles, not magic)

Specialization beats one big agent — Buttercup's component split and our
own PoC both point this way. Roles share the same runtime; they differ in
system prompt, tool access, and budget:

- **scout** — maps attack surface at the pinned head: endpoints, state
  machines, spec diffs, past-bug archeology (git log, advisories).
  Output: hypothesis queue entries.
- **operator** — takes one hypothesis, writes and iterates on a PoC in
  the sandbox with oracle feedback. Output: demonstrated result or
  exhaustion of budget.
- **critic** — independent re-verification: replays the PoC from the
  transcript (fresh sandbox), checks novelty (does it reproduce on the
  previous release?), dedupes against existing findings. A finding
  graduates to `verified` only through the critic.
- **librarian** — distills verified work into the corpus: finding files,
  technique cards, threat-model updates, regression tests.

v1: one researcher at a time, role switching by phase. Parallel
researchers need one environment instance each (or strict serialization);
environment pooling is a v2 concern.

## Plugin protocol (the most important API in the project)

A plugin is an executable speaking newline-delimited JSON on stdio —
git-remote-helper style. Any language; a bash script qualifies.

```
→ {"id":1,"method":"probe"}
← {"id":1,"ok":true,"result":{"ready":true,"notes":"regtest up"}}
→ {"id":2,"method":"up"}
← {"id":2,"ok":true}
→ {"id":3,"method":"targets"}
← {"id":3,"ok":true,"result":["http://corpus-target-gw:8085","http://corpus-target-gw:8087"]}
→ {"id":4,"method":"sandbox_exec","params":{"command":"echo hi"}}
← {"id":4,"ok":true,"result":{"output":"hi","exit_code":0}}
→ {"id":5,"method":"oracles"}
← {"id":5,"ok":true,"result":[{"name":"020-conservation","description":"conservation of value"}]}
→ {"id":6,"method":"call_oracle","params":{"name":"020-conservation"}}
← {"id":6,"ok":true,"result":{"verdict":"hold","log":"..."}}
→ {"id":7,"method":"faucet","params":{"op":"pay","invoice":"lnbcrt123..."}}
← {"id":7,"ok":true,"result":{"text":"paid 100 sat","paid_sats":100}}
```

Methods (v1):

| method | purpose |
|---|---|
| `probe` | is the environment available? cheap health check |
| `up` / `down` | lifecycle |
| `reset` / `snapshot` / `restore` | deterministic state for replay (future) |
| `targets` | scoped endpoints the agent may attack (allowlist) |
| `tools` | files/dirs mounted read-only into the sandbox |
| `sandbox_exec` | run a command in the plugin-owned sandbox; returns `{output, exit_code}` |
| `oracles` / `call_oracle` | invariant listing and execution; returns `{verdict, log}` |
| `faucet` | pay / create / balance a regtest Ln invoice; returns `{text, paid_sats?}` |
| `attack_surface` | hints: endpoints, state machines, auth boundaries (future) |
| `threat_model` | seed document the research job keeps current (future) |

Security notes:
- Plugins run host-side and are trusted — they orchestrate docker/nix.
  Least privilege where easy, but no illusion of sandboxing.
- `targets` is a hard scope boundary: the sandbox network policy is
  derived from it, so an agent physically cannot stray out of scope.
- The plugin owns the long-lived sandbox container; corpus never learns
  its name. `sandbox_exec` lazy-starts it, so the harness is up on first
  use without an explicit boot step.
- Enforcement split: the **plugin** enforces environment policy (per-job
  egress, per-payment cap, regtest-only) and corpus enforces **corpus
  policy** (per-session budget, finding gate). Neither can talk its way
  around the other's checks.
- Plugin distribution: a registry is just a git repo of manifests
  (name, url, rev, checksum). Signing manifests is cheap; do it early.

The reference plugin is `cdk-regtest` in `plugins/`, absorbed from the
vul-lab harness (2026-08-10) as the first-party plugin: `arena.sh` manages
the sandbox/gateway networks, `oracles/` are the invariant suite, and
`faucet.sh` is the regtest Lightning faucet. corpus ships with it working
on day one, and is fully self-contained — no dependency on the old vul-lab
directory. `setup.sh` builds the agent image, compiles the attack tools,
and runs the doctor self-verification probes.

## Trust domains (hard rules)

1. **Execution sandbox** — runs attack code, egress denied by default,
   resource-capped, read-only root fs. Egress is a per-job config switch
   because experiments demand it, with the default always off.
2. **Research zone** — reads the internet (CVE feeds, advisories, specs),
   executes nothing. Its output is untrusted text input to the testing
   pipeline. (Prompt-injection containment; see CAI's defense paper.)
3. **Model inference** — host-side, local by default. The sandbox has no
   model access; only tool execution crosses the boundary.
4. **Corpus store** — private git repo, signed commits, optional
   git-crypt/age encryption. Findings carry embargo metadata.

## The corpus data model

Git repository, markdown + YAML frontmatter, one directory per class:

```
corpus/
  targets/cdk/             # threat model, attack surface, env recipe ref
  techniques/              # ATT&CK-style cards: "quote race", "fee rounding"
  findings/CDK-2026-0001/  # frontmatter (cwe, osv-style ranges, severity,
    finding.md             #   status: candidate|verified|disclosed|fixed)
    poc/                   # executable repro — doubles as regression test
    run.jsonl              # hash-chained transcript excerpt
  oracles/                 # shared invariant library
  benchmarks/              # forensic suite (below)
  missions/                # reusable mission briefs
```

Why git: reviewable diffs, branches = embargoes, signatures = provenance,
any private remote = backup/sharing, and LLMs are natively fluent in it.

## Meticulous logging

Every run appends to `run.jsonl`: model request/response hashes, tool
calls and results, environment identity (plugin version, target rev,
container image digest), timestamps, verdicts. Each entry chains the
previous entry's hash; the run head is committed (signed) to the corpus.

This gives: replayable sessions, auditable provenance for disclosure
reports ("here is exactly what the machine did"), and regression data for
model swaps ("did qwen X.Y get worse at mission 002?").

## The two jobs

**Campaign** (ad hoc): `corpus run missions/003-quote-abuse.md --target cdk`.
Scout → operator → critic → librarian, one mission, bounded budget.

**Watch** (continuous): `corpus watch --target cdk`.
- New commits on the pinned target → replay all verified PoCs (free
  regression suite), diff-scope a scout pass, emit new hypotheses.
- External feeds (OSV, NUT spec changes, sibling-project advisories) →
  research zone distills technique cards → new missions enter the queue.
- The corpus's attack-coverage matrix (personas × surfaces × techniques)
  drives what gets attacked next, so effort goes where coverage is stale.

## Forensic benchmarking (how we know it works)

Seed the benchmark suite with history: pin the target to the parent commit
of a known past fix (cashu/cdk and LN implementations have public
security-fix history), and check whether researchers rediscover the bug —
without being told where it is. This yields:

- a true-positive rate we can measure per model/version/config change
- regression detection when swapping local models
- a demo that is safe to show (the bugs are already fixed upstream)

CAIBench, oss-fuzz-gen's benchmark-sets, and Strix's benchmarks all exist
because serious projects in this space are measured. Ours is distinguished
by end-to-end verifiability: not "did the fuzzer crash" but "did the
oracle trip."

## The model lab

corpus treats models as benchmarked, tagged equipment — not a config
string. Three artifacts:

- **Model registry** (`models.yaml`): every model worth tracking, with
  capability tags (`coding`, `tool-use`, `long-context`, `reasoning`),
  parameter size, context window, provider, and install status.
- **Forensic benchmark suite** (`benchmarks/forensic/`): historical bugs
  at pinned parent commits (see below), run per model with fixed harness
  and budgets. Scores: found/not-found, steps, wall time, oracle verdict.
- **Results matrix** (`benchmarks/results/<model>/<suite>.yaml`): every
  run recorded, so model swaps are regression-tested and new releases are
  evaluated against *your* task families the day they drop.

The unit of comparison is purpose-built task families — funded LN
attacks, quote state-machine abuse, race conditions, auth probing — not
generic coding benchmarks. A model that is mediocre at HumanEval but
great at racing swap requests is the model you want; corpus is how you
find that out.

Provenance: every finding and run carries the exact model tag (and
inference config: num_ctx, temperature) that produced it. Claims about
model capability without the run logs to back them are marketing.

## Frontends

**TUI first** (ratatui): run dashboard (live transcript tail, budget,
oracle status), findings review queue (verify/embargo/export), mission
editor, coverage matrix. Developers are the operators; the terminal is
home.

**Desktop later** (Tauri): same `corpus-core` API, better at graph views
(coverage matrix, finding timelines, technique relationships) and at the
review/approval workflow for disclosure. If pure Rust is preferred, egui
is the fallback; decide when we get there.

## Roadmap

0. **Extract** ✅ — plugin protocol spec + the `cdk-regtest` reference
   plugin, absorbed from the vul-lab harness (2026-08-10) as the first
   party `plugins/cdk-regtest`. Protocol extended with `sandbox_exec`,
   `faucet`, and `tools`; `call_oracle` returns verdict + log. The manual
   alpha-1 flow — `target_info` → `wallet_fund` → `oracle_run` →
   `attack_save` → `finding_write` — runs entirely through the plugin
   protocol today.
1. **Store**: corpus repo layout, finding schema, run logging with
   hash-chaining and signed commits.
2. **Team**: roles (critic first — it is the verification multiplier),
   watch mode skeleton, forensic benchmark suite v1 (3-5 historical bugs).
3. **TUI**: run dashboard + findings review.
4. **Community**: plugin authoring guide + registry repo; desktop app.

## Open questions

- **Name**: "corpus" collides with the fuzzing term and existing projects;
  check availability before publishing (crate name, brew formula, domain).
- **Encryption UX**: git-crypt vs age-files vs per-repo disk encryption —
  pick the one that keeps `git diff` usable for authorized reviewers.
- **Frontier API opt-in**: worth supporting with a redaction layer (strip
  target identity), or does it violate the project's core promise?
  Leaning: support it, default off, loudly labeled.
- **Multi-target parallelism**: environment pooling vs serialize-per-env.
  Start serialized; revisit when campaigns queue up.
- **Embargo workflow export**: GitHub private security advisories API vs
  signed email bundles; needed before the first real external finding.
