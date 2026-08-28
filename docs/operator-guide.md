# Operator guide

This guide covers routine operation of Corpus. It assumes the local host and
operating-system account are trusted and every target is owned by the operator
or explicitly authorized for testing. See [`threat-model.md`](threat-model.md)
for the security contract and [`troubleshooting.md`](troubleshooting.md) when a
workflow fails.

## Install and verify

```sh
cargo build --locked -p corpus-cli -p corpus-admin-mcp -p corpus-mcp
cargo build --locked -p corpus-app
```

Embedded management chat is enabled by default. A headless build can omit its
Goose dependency tree with `cargo build --locked -p corpus-app
--no-default-features`.

Corpus does not ship a production environment adapter in this repository.
The desktop app's **New project** dialog lists the supported environments and
installs one without leaving Corpus. Corpus downloads the public release,
checks it against the built-in catalog, installs an immutable copy, and selects
that version. [`../PLUGINS.md`](../PLUGINS.md) owns installation,
upgrade/rollback, and authoring details.

```sh
corpus plugin install cdk-regtest
corpus plugin setup <plugin>
corpus plugin doctor <plugin>
corpus plugin status <plugin>
```

`setup` may fetch manifest-pinned sources and build local images. Plugins that
declare Docker support surface Docker readiness through their environment
status. `doctor` is the full read-only readiness check; `status` is the faster
lifecycle view. Plugin authors can install an unpacked development bundle with
`corpus plugin install-local /path/to/bundle`.

## Data and configuration

Corpus stores operator data beneath `$CORPUS_HOME`, defaulting to `~/.corpus`:

```text
store/projects/<project>/     project, agent, mission, and corpus records
plugins/                      immutable installed plugin versions
var/run/<project>/            project-confined run workspace
var/chat/<project>/           management-chat sessions and diagnostics
var/audit/<project>.jsonl     curator actions
var/refusals/<project>.jsonl  rejected MCP calls
app.yaml                      desktop preferences
```

Supported path overrides include `CORPUS_HOME`, `CORPUS_STORE`,
`CORPUS_RESOURCES`, `CORPUS_PLUGINS_DIR`, `CORPUS_SOURCES_DIR`, and
`CORPUS_MODELS`. `CORPUS_PLUGINS_DIR` replaces the complete plugin catalog and
is for development and tests. Do not point it at an unreviewed checkout during
normal operation. The resource root is shipped read-only data; the store is
operator-owned mutable data. See [`architecture.md`](architecture.md).

## Create a project and mission

Use the desktop application for the guided workflow, or the typed CLI:

```sh
corpus project new example --name "Example" --plugin <plugin>
corpus agent new example lead --role curator
corpus agent new example tester --role tester
corpus mission new example first-pass --agent tester \
  --budget 30m "Investigate the authorized target"
```

Roles are capability ceilings: `researcher` researches without execution;
`tester` uses the plugin sandbox without open research authority; `curator`
manages only its proven project; and `super` combines project-scoped roles but
is not a host-global operator. Every role can persist durable work through
`entry_write` at any relative path beneath its own corpus. Agents may organize
that data into categories that fit the work; `runs/` remains immutable, and
cross-project, absolute, traversal, and symlink escapes are refused.

Source revisions are resolved when a mission is created. A repeated
`--pin source=revision` overrides the project pin, which overrides the plugin
default. Launch models are always explicit; Corpus does not use an ambient
OpenCode default.

```sh
CORPUS_PROJECT=example corpus run tester -m <provider/model> \
  "Investigate the authorized target"
```

The desktop application normally uses an attachable tmux-backed session in its
embedded terminal. `CORPUS_NO_TMUX=1` forces the piped backend. Mission
teardown exports the transcript when possible; raw capture begins with first
output and survives app exit or an export failure.

## Curator orchestration

A curator may create and launch project missions, continue management work,
and receive completion notifications addressed to its exact originating
session. It uses `mission_status` for an intentional snapshot, not polling.
Waiting and completion delivery are app-owned so a model turn is not kept open
merely to supervise a child.

Durable mission and environment identities let completion delivery and cleanup
resume after restart. Do not manually remove their records to clear a stuck
display; use the recovery controls below.

## Management chat

The desktop app embeds Goose behind Corpus-owned interfaces. Operator mode has
the full administration catalog. Ordinary writes require inline approval by
default, destructive calls always require approval, and specialists receive
only their declared tool subsets. Delegated specialist calls retain the same
policy.

The optional Goose CLI fallback must run through `scripts/goose-chat`, which
confines configuration and sessions to the explicit project:

```sh
CORPUS_PROJECT=example scripts/goose-chat -n ops
CORPUS_PROJECT=example scripts/goose-chat run -t "list this project's missions"
```

Do not use raw `goose run` for Corpus administration. Its default session
location is outside project scope. `CORPUS_CHAT_APPROVE_WRITES=0` may disable
approval for ordinary writes during controlled local work; it never disables
destructive approval. Unknown tools fail closed.

## Destructive operations and cleanup

The administration MCP tools for project, agent, mission, corpus, and entry
deletion use a short-lived, single-use server confirmation token. The first
call returns a dry-run summary; the confirmed second call mutates. Desktop chat
adds an earlier approval gate and does not expose the token to the model. The
direct typed CLI is an explicit host-operator surface and does not use this
two-call MCP ritual.

Deleting an object with a live tmux or plugin environment records a durable
request. The app removes the record only after teardown is proven. If cleanup
fails:

1. Open **Project → Configuration**.
2. Inspect the retained environment identity and error.
3. Correct the plugin or host problem.
4. Choose **Retry cleanup**.

Do not stop a plugin while it has live mission leases. The refusal preserves
the identity needed for recovery.

## Audit and diagnostics

Every scoped curator mutation records intent and outcome. Every MCP error that
reaches an agent is recorded best-effort in the refusal log.

```sh
corpus audit <project> --tail 50
corpus refusals <project> --tail 50
corpus refusals <project> --gate scope
```

Refusal gates are `identity`, `role`, `scope`, `probe`, `args`, `unknown`, and
`harness`. Calls rejected before project identity is established land under
`_unscoped`. Start with this structured log before a raw PTY capture; it holds
the exact gate, actor, arguments, error, and correlated run-log basename.

Chat transcripts and diagnostics live under `var/chat/<project>/`; run evidence
lives in the project's `corpus/runs/`. These can contain sensitive findings.
Preserve project scope and sensitivity classification when sharing them.

## Safe upgrade and rollback

Before upgrading Corpus or a plugin:

1. Finish or deliberately stop active missions.
2. Preserve the store and correlated run evidence.
3. Install a new immutable plugin version; never overwrite an old bundle.
4. Run plugin `doctor` before selecting it for future work.
5. Run the appropriate verification tier from [`testing.md`](testing.md).

### Probe namespace migration

Corpus stores executable regression artifacts under
`corpus/probes/<slug>/{probe.md,run.sh}`. Projects created before this change
may still contain `corpus/attacks/<slug>/{attack.md,run.sh}`. Preview and apply
the per-project migration explicitly:

```sh
corpus project migrate-probes <project>
corpus project migrate-probes <project> --apply
```

The first form is read-only. Migration refuses symlinks, malformed legacy
artifacts, and projects with non-empty entries in both namespaces. During the
compatibility window, legacy entries remain readable and deletable and the
deprecated `attack_save` MCP alias writes through to the new `probes/`
namespace. New generic writes under `attacks/` are refused.

Plugin selection affects future sessions only. Existing evidence retains its
plugin, bundle, environment, image, source, model, and agent identities.
