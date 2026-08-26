# Troubleshooting Corpus

Start from the observed symptom. Do not delete durable mission, environment,
audit, refusal, or run records to hide one; they are evidence and recovery
identity. Routine workflows are in [`operator-guide.md`](operator-guide.md),
and test-runner failures in [`testing.md`](testing.md).

## First capture

Before retrying, retain the first-attempt transcript, raw capture, mission
record, and chat log, then record:

```sh
corpus plugin status <plugin>
corpus plugin doctor <plugin>
corpus audit <project> --tail 50
corpus refusals <project> --tail 50
```

A retry is diagnostic evidence, not a replacement for the first attempt.

## A tool call was refused

Read `corpus refusals <project>` before the PTY transcript:

| Gate | Meaning | First check |
|---|---|---|
| `identity` | Project, agent, mission, or run identity is inconsistent | Launch environment and mission origin |
| `role` | The server-derived role lacks the capability | Agent role and advertised catalog |
| `scope` | The request escaped proven project or filesystem scope | Project binding and relative path |
| `probe` | The environment is not ready | `corpus plugin doctor <plugin>` |
| `args` | Typed arguments or confirmation are invalid | Schema, exact target, fresh token |
| `unknown` | The server does not know the tool | Client/server version and catalog |
| `harness` | The run harness rejected or failed it | Correlated run log and launch diagnostics |

An empty refusal log is useful: Corpus MCP did not reject the call. Check the
rendered OpenCode policy, client-side tool availability, or a mistaken path.
`/opt/src/...` is a sandbox mount reached through sandbox tools; host-side
agents see sources under the run workspace's `sources/...` tree. Pre-identity
failures are available with `corpus refusals _unscoped --tail 50`.

## A mission will not launch

Check in order:

1. The project exists and is explicitly selected.
2. The agent exists and its project delegation graph is closed.
3. Launch has an explicit provider/model identifier.
4. Every source pin resolves to its stored immutable revision.
5. The selected plugin is installed, verified, and ready.
6. No non-closed run generation already owns the mission.

```sh
corpus project list
corpus agent list <project>
corpus mission list <project>
corpus plugin list
corpus plugin doctor <plugin>
```

If tmux is unusable, `CORPUS_NO_TMUX=1` distinguishes a terminal-host problem
from launch or model behavior. Piped mode does not repair plugin or identity
failures.

## A run is alive but the app cannot attach

Relaunch Corpus and use its live-session entry. If attach alone fails, confirm
tmux 3.2a or newer is available to the app, correlate by durable mission
identity, and inspect the raw capture under the project's `corpus/runs/`.
Preserve the original session before starting a piped diagnostic run.

Do not kill an arbitrary `corpus-*` session by display name. Teardown is
identity-bound so it cannot terminate a newer mission with a reused label.

## Deletion or cleanup is stuck

A retained record after deletion was requested means teardown has not proved
its postcondition. Open **Project → Configuration**, inspect the recorded
error, correct the host/plugin condition, and choose **Retry cleanup**.

Common causes are live Docker resources, plugin timeout, mismatched selected
plugin version, or loss of access to the runtime owning the session. Plugin
stop correctly refuses while mission leases remain. Never delete the lease or
mission record by hand; it is needed to close the resource safely.

## A plugin is missing or unhealthy

If `corpus plugin list` is empty, verify the bundle was installed rather than
merely checked out, inspect `$CORPUS_HOME/plugins`, and unset an accidental
`CORPUS_PLUGINS_DIR` catalog override. Verify checksum and manifest before
reinstalling.

```sh
corpus plugin status <plugin>
corpus plugin doctor <plugin>
corpus plugin probe <plugin>
```

`status` reads lifecycle state, `doctor` performs readiness checks, and `probe`
exercises protocol health. Capture doctor output before using the lower-level
`corpus plugin call <plugin> <method> [params-json]`. Plugin-specific Docker,
oracle, and source failures belong to the independent plugin bundle; see
[`../PLUGINS.md`](../PLUGINS.md).

## Management chat fails or loses scope

The desktop app needs the default `chat-embed` feature; a
`--no-default-features` build intentionally has a no-op backend. For CLI
fallback, build `corpus-admin-mcp`, set `CORPUS_PROJECT`, and use
`scripts/goose-chat`, never raw `goose run`. Override `CORPUS_ADMIN_MCP` only
when the wrapper cannot find the intended binary.

If embedded Goose fails to compile, use the Rust toolchain pinned by its source
checkout. If global Git configuration rewrites GitHub HTTPS to SSH, perform the
authorized fetch with that rewrite disabled; do not change the pinned Goose
revision as a repair.

## A model is missing or wrong

Mission launches require an explicit model and never fall back to OpenCode's
ambient default. Refresh desktop model discovery and verify the provider/model
identifier in the same process environment.

Live integration tests accept only `qwen3.8:27b-mlx`, verify configured
identity, and acquire the global cross-process lease. Never start a second
live-model test in parallel, even from another Cargo process. Use only the
prepared-runner commands in [`testing.md`](testing.md).

## Build and dependency failures

Start with the hermetic, Goose-free path:

```sh
cargo build --locked -p corpus-app --no-default-features
cargo test --locked --workspace --all-targets
./scripts/check-dependency-policy
./scripts/check-supply-chain
```

Do not fix dependency-policy or supply-chain failures with broad exceptions.
Update the owner or the dated rationale in
[`supply-chain-policy.md`](supply-chain-policy.md).

## Escalation bundle

Preserve exact Corpus, OS, tmux, OpenCode, plugin, and model versions; project,
mission, run, plugin, and model identities; plugin status/doctor output; audit
and refusal tails; first-attempt transcript/raw capture; chat transcript/log;
and the earliest error. Redact secrets and honor sensitivity classifications,
but retain identity fields needed for correlation.

