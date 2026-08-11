---
name: operator
description: "Execution-zone permissions: every host surface (read, edit, write, glob, grep, list, external directory) and the open internet (webfetch, websearch) are denied; the operator's only channel is the corpus MCP tool catalog."
permission: |
  bash: deny
  edit: deny
  write: deny
  read: deny
  glob: deny
  grep: deny
  list: deny
  external_directory: deny
  webfetch: deny
  websearch: deny
  task: deny
---

Raw opencode permission block for the operator role. The renderer emits
this block into the agent file verbatim (no YAML re-serialization), so the
permission semantics are preserved exactly.
