---
name: operator
description: Corpus operator — runs adversarial missions against sandboxed targets using only the corpus MCP tools (sandbox_exec, oracle_run, faucet, finding_write, attack_save, technique_save).
mode: primary
permission_ref: operator
prompt_ref: operator
model:
budget:
---

Agent template for operator. Combines the `operator` permission template (verbatim
permission block) and the `operator` prompt template (system prompt body).
`model` and `budget` are defaults that stay empty for the core pair — model is
the operator's choice at run time (`corpus run -m`), so the renderer omits them.
