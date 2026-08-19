# corpus-admin Goose CLI recipes (script-only)

These files are for the optional `scripts/goose-chat` fallback and are not
loaded or packaged by `corpus-app`; the app uses its embedded chat runtime.

The corpus-admin management recipe (dev/gdk-chat-plan chunk 2). The role
prompts live inline in each recipe YAML (`instructions`); this README is the
human-facing overview. If prose here drifts from the YAML, the YAML wins.

## THE ENFORCEMENT VERDICT (D1) — read this first

**goose subrecipe delegation does NOT enforce per-subagent tool grants.**
In goose CLI 1.46.0, a subrecipe's `extensions:` block (including
`available_tools`) is NOT applied to the subagent it spawns — a delegated
subagent receives only the `summon` tools (`load` / `delegate`). Evidence:
the orchestrator catalog surfaces only `delegate`+`load`, and every delegated
subagent's catalog is only `load`, across all probe runs (transcripts + LLM
request logs in `store/projects/<p>/var/chat/`, session ids in dev/gdk-chat-plan
Task-2).

Consequence per the plan: the team design **degrades to a flat single agent
with the full admin catalog, with the confirm-token gate as the sole hard
control**. That is what `recipe.yaml` ships (one agent, all corpus-admin
tools, dry-run + confirm token on every destructive op). The five-role design
becomes internal routing on that single agent.

`available_tools` filtering IS real at the **recipe-as-main** level (positive
control: running `subrecipes/corpus-inspector.yaml` as its own main recipe
loaded exactly its 6 read-only tools). So real per-domain isolation remains
possible if chunk 3 runs each specialist as its OWN separately-scoped goose
session rather than as a goose subrecipe. The subrecipe files below document
those grants for that option.

## Layout

- `recipe.yaml` — the shipped (degrade-arm) management recipe: ONE agent,
  full corpus-admin catalog, confirm-token gate. All five roles as routing.
- `subrecipes/*.yaml` — the four-specialist specs. **Advisory in goose
  delegation** (see verdict); enforceable only when run as their own main
  recipe. Kept as the team-spec + a fallback for per-domain scoped sessions.
- `prompts/README.md` — this file.

## Team (per dev/gdk-chat-plan §"The agent team")

| role | owns | intended tool grant (`available_tools`) |
|---|---|---|
| **orchestrator** | intent routing, clarification, confirmation ritual, final summaries | NO admin tools |
| **agent-builder** | create by role; edit validated opencode.json | `agent_list/get/new/save/clone/delete`, `corpus_read`, `model_list` |
| **project-manager** | new/clone/rebind/delete/wipe | `project_list/new/clone/delete/rebind`, `corpus_wipe`, `corpus_stats` |
| **mission-manager** | mission CRUD, budget, pins; agent→mission budget | `mission_list/get/new/delete/set_budget/set_pins`, `agent_list` |
| **corpus-inspector** | read-only store queries | `corpus_stats/list/read`, `project_list`, `agent_list`, `mission_list` |

## Vocabulary (baked into the recipe)

- **budget** → per-MISSION, never per-agent. "increase the budget for agent z"
  = find z's mission(s), disambiguate when >1, edit frontmatter.
- **delete this corpus** → `corpus_wipe`: dry-run + one-shot confirm token,
  mutation only on re-call with the token.
- **copy project, target P** → `project_clone` (+ optional corpus) then
  `project_rebind` (registry-validated) and/or agent-builder edits; clarify
  whether "P" is the plugin or the agents' briefs.
- Slugs **kebab-case**; every corpus entry carries `sensitivity:`.
