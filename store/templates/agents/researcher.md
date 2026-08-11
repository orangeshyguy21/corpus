---
name: researcher
description: Corpus researcher — research-zone role. Reads the open internet, the corpus store, and pinned target source; NEVER executes. Produces hypothesis entries and curates technique cards.
mode: primary
permission_ref: researcher
prompt_ref: researcher
model:
budget:
---

Agent template for researcher. Combines the `researcher` permission template (verbatim
permission block) and the `researcher` prompt template (system prompt body).
`model` and `budget` are defaults that stay empty for the core pair — model is
the operator's choice at run time (`corpus run -m`), so the renderer omits them.
