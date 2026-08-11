---
name: researcher
description: "Research-zone permissions: no execution (bash, task, sandbox, faucet, wallet_fund, oracle_run), webfetch/websearch allowed, team-scoped corpus writes allowed, benchmarks/ reads denied, project-corpus writes and promotion denied, finding gate denied, target_info + technique_save allowed."
permission: |
  bash: deny
  task: deny
  webfetch: allow
  websearch: allow
  write:
    "*": deny
    "store/projects/*/teams/*/corpus/**": allow
  edit:
    "*": deny
    "store/projects/*/teams/*/corpus/**": allow
  read:
    "*": allow
    "benchmarks/**": deny
  corpus_sandbox_exec: deny
  corpus_faucet: deny
  corpus_wallet_fund: deny
  corpus_oracle_run: deny
  corpus_finding_write: deny
  corpus_attack_save: deny
  corpus_promote: deny
  corpus_target_info: allow
  corpus_technique_save: allow
---

Raw opencode permission block for the researcher role. The renderer emits
this block into the agent file verbatim (no YAML re-serialization), so the
permission semantics are preserved exactly.

Write/edit access is deliberately scoped to the TEAM corpus
(`store/projects/*/teams/*/corpus/**`): opencode wildcards are
character-based (`*` matches any run of characters, including `/`), so the
literal `/teams/` segment of the pattern is what pins it to team corpora —
a project-global corpus path (`store/projects/<p>/corpus/…`) has no
`/teams/` and cannot match. That closes the promotion-gate bypass: the
researcher can no longer write the project-global corpus directly; entries
reach it only via `corpus_promote`, which is denied here.
