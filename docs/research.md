# Research landscape

Surveyed 2026-08. Purpose: map what exists, steal what works, and identify
the gap corpus should occupy. Links verified against the projects' own
repos/docs where possible.

## The autonomous vulnerability-discovery space

### Buttercup — Trail of Bits (AIxCC finalist, open source, AGPL-3.0)
<https://github.com/trailofbits/buttercup>

A full Cyber Reasoning System built for DARPA's AI Cyber Challenge:
orchestrator, LLM-assisted seed generation, fuzzing (built on oss-fuzz),
program model (code structure analysis), and a multi-agent patcher.
Kubernetes-based deployment, SigNoz observability, Langfuse LLM cost
tracking.

Lessons:
- **Its scope is narrow by design**: C/Java repos that are oss-fuzz
  compatible with existing harnesses. It finds memory bugs, not logic or
  protocol bugs. There is no concept of a live system under test.
- **Cloud-scale and paid-model-dependent.** The README warns that tasking
  it consumes third-party AI spend. Not local-first, not private.
- **Patching is half the product.** corpus is deliberately find+verify
  first; a "propose patch" stage is a natural later plugin.
- The multi-component split (seed-gen / fuzzer / program-model / patcher)
  validates a *roles* architecture over one monolithic agent.

### oss-fuzz-gen — Google (open source, Apache-2.0)
<https://github.com/google/oss-fuzz-gen>

LLM-generated fuzz harnesses, benchmarked at scale: 1300+ benchmarks
across 297 projects, 30+ real bugs reported (including CVE-2024-9143 in
OpenSSL), evaluated on compilability / crashes / coverage delta.

Lessons:
- **Benchmarks are table stakes.** It has `benchmark-sets/`; CAI has
  CAIBench; Strix has `benchmarks/`. corpus needs its own evaluation loop
  from day one (see "forensic benchmarking" in architecture.md).
- **"Reports are not public as they may contain undisclosed
  vulnerabilities."** Even Google keeps AI-found bugs private. The private
  corpus store isn't paranoia; it's industry practice.
- LLM-written *harnesses* are a force multiplier for fuzzing; a corpus
  researcher can generate new fuzz targets as a side effect of campaigns.

### Big Sleep / Project Naptime — Google Project Zero + DeepMind
<https://googleprojectzero.blogspot.com/2024/10/from-naptime-to-big-sleep.html>
<https://googleprojectzero.blogspot.com/2024/06/project-naptime.html>

The landmark result: an LLM agent found a previously unknown, exploitable
stack buffer underflow in SQLite — before release, reported and fixed the
same day. 150 CPU-hours of AFL on the same target found nothing.

Lessons (the most technically relevant paper to corpus):
- **Variant analysis is the LLM sweet spot.** "This was a previous bug;
  there is probably another similar one." Given a seed (commit diff, past
  advisory), agents produce concrete, well-founded hypotheses. This is
  exactly the *research job* corpus runs continuously: the corpus of past
  findings and external advisories becomes the seed library.
- **The trajectory looks like a human researcher**: form hypothesis →
  probe → hit setback → adapt tooling → reproduce → only then write the
  root-cause report. The loop matters more than the prompt.
- **Tooling over prompting**: code browser, debugger, testcase runner.
  corpus's equivalent: sandbox shell, project CLI tools, oracle runner.
- Their own caveat stands: a target-specific fuzzer may be as effective
  for memory bugs. corpus targets the classes fuzzers *can't* express:
  protocol logic, state machines, economic invariants.

### CAI (Cybersecurity AI) — Alias Robotics (open source, 9.7k stars)
<https://github.com/aliasrobotics/cai>

Python framework for offensive/defensive agents: agent patterns and
handoffs, 300+ models via LiteLLM (incl. Ollama), built-in offensive
toolkit, guardrails against prompt injection (they published a
multi-layer defense paper, arXiv:2508.21669), human-in-the-loop, tracing
via Phoenix. Strong CTF/bug-bounty track record; commercial pro edition.

Lessons:
- **Prompt injection is a first-class threat** when agents ingest external
  content (their paper, and our own design split: research zone reads the
  internet, testing zone executes code, never both at once).
- LiteLLM-style model abstraction is worth copying in spirit: one
  OpenAI-compatible trait, swappable backends.
- Python + framework = it assembles agents; it is not a persistent,
  verifiable knowledge product. No pinned-head reproducibility, no
  oracle/ground-truth concept.

### Strix (open source, Apache-2.0, 50k stars)
<https://github.com/usestrix/strix>

Autonomous AI pentesting for apps: Docker sandboxes, multi-agent "graph",
real PoC validation, local web viewer (`strix view` — reads run files off
disk, "nothing leaves your machine"), CI/CD with diff-scoped PR scans,
agent-consumable SKILL.md docs. Cloud platform upsell.

Lessons:
- **PoC validation as marketing and as engineering**: "working PoCs, not
  false positives" is the demand signal in the market. corpus's oracle
  verification is the stronger version of this claim.
- The local viewer shows developers want offline-capable result
  inspection. A TUI is the bitcoin-native version of that.
- Web/API focus: no regtest, no protocol economics, no custom harness
  concept. Their plugin surface is "skills"; ours is whole environments.
- 50k stars means the category is crowded at the *web app* layer —
  another reason to own the systems/bitcoin niche instead.

### Closed-source / hosted
XBOW, ZeroPath, Horizon3, Runsybil, Xint, and ~20 others (CAI maintains a
list on their README). Hosted, web-centric, findings leave your
infrastructure. They validate demand; they don't occupy the local-first,
verification-grounded niche.

## Enabling technology choices

### Language: Rust
Single static binary, the ecosystem the target audience (bitcoin infra
devs) already reviews, memory safety for a tool that itself handles
exploit material, and first-class TUI/desktop options. The plugin protocol
keeps community plugins language-agnostic.

### TUI: ratatui
<https://github.com/ratatui/ratatui>
The de-facto Rust TUI framework (active, huge ecosystem). Devs running
long-lived research campaigns live in terminals and tmux; a TUI is not a
compromise, it is the primary interface. Crossterm backend covers macOS
and Linux.

### Desktop: Tauri (later), egui (pure-Rust alternative)
<https://tauri.app> · <https://github.com/emilk/egui>
Tauri: system webview, small binaries, mac/linux packaging, good for
graph-heavy views (attack-coverage matrix, finding timelines) reusing web
viz libs. egui: single-codebase pure Rust, no webview dependency, but
weaker rich-viz ecosystem. Recommendation: **TUI first, desktop later** —
both sit on the same `corpus-core` library, so the desktop app is a shell,
not a rewrite.

### Plugin protocol: subprocess + JSONL (git-remote-helper style)
Plugins control host infrastructure (docker, nix, Lightning nodes) — they
are trusted code by nature, so WASM (Extism) buys little here and costs
authors a lot. A subprocess speaking newline-delimited JSON over
stdin/stdout is language-agnostic (a plugin can be a bash script), trivial
to debug (`corpus plugin call ... | jq`), and matches how git-remote
helpers and terraform providers have thrived. Sandboxing: run plugins with
least privilege, but assume trust.

### Knowledge store: git + encryption + signatures
The corpus is a git repository: human-reviewable diffs, branching for
embargoes, signed commits for provenance, trivial sync to a private
remote. Encrypt with git-crypt or age-encrypted files for at-rest
protection. LLMs read and write plain git well — no database server to
operate, no schema migrations.

### Vulnerability metadata: steal from OSV/SARIF/CWE
- OSV schema (<https://ossf.github.io/osv-schema/>) for finding identity
  and affected-version ranges
- CWE for weakness classification
- SARIF (<https://sarifweb.azurewebsites.net/>) interop if we ever emit to
  GitHub code scanning etc.
Adopt fields, not formats: the corpus is markdown + YAML frontmatter,
exportable to OSV/SARIF.

### Sandboxing: docker/containers now, microVMs later
vul-lab's proven model: internal docker network + locked-down container +
host-side oracles. Firecracker/microVMs (e.g. via `microsandbox`-style
projects) are the upgrade path for stronger isolation; not needed for PoC.

### Local inference: ollama / OpenAI-compatible
Proven in vul-lab with a 35B open-weight model running multi-step attack
missions. Requirements learned the hard way: raise `num_ctx` (ollama
defaults to 4k and silently truncates), pin `keep_alive` (idle unload
mid-mission), warm the model before the run.

## The gap corpus occupies

| | Buttercup | oss-fuzz-gen | CAI | Strix | **corpus** |
|---|---|---|---|---|---|
| Open source | yes | yes | yes | yes | yes |
| Local / private findings | no | no | partial | partial | **yes** |
| Open-weight local models | no | no | yes | yes | **yes** |
| Systems/protocol targets | no (C/Java fuzz) | no | general | web/apps | **yes** |
| Live environment with economic ground truth | no | no | no | no | **yes (regtest)** |
| Executable invariant verification | crash-only | crash-only | PoC | PoC | **oracle suite** |
| Persistent knowledge corpus | no | no | no | partial | **yes (git, signed)** |
| Pinned-commit reproducibility | partial | yes | no | partial | **yes** |
| Community env plugins | no | no | tools only | skills | **yes** |

The wedge: **bitcoin/ecash infrastructure**, where regtest gives
verification superpowers no generic tool has, and where privacy of
findings is non-negotiable. If it works there, it generalizes to any
project with a reproducible test environment.

## First-hand lessons imported from vul-lab (the PoC)

1. Verification-first survives contact: two real harness bugs (docker
   `--internal` blocking the host gateway; an oracle asserting a non-spec
   field) were caught by self-probes, not by reading code.
2. The agent will discover your harness's gaps for you — mission 001
   autonomously found it couldn't pay invoices, which became the faucet.
3. Weaker local models flail without feedback; with oracle feedback and a
   step budget, a 35B model ran a coherent 25-step funded attack mission.
4. Model operations are a reliability feature, not an afterthought
   (context size, keep-alive, warm-up).
5. Every finding's PoC is a free regression test — the system's output
   compounds.
