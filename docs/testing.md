# Test suites

Corpus separates tests by the services and process capabilities they require.
The default workspace test command must stay hermetic: it may use temporary
files and controlled child processes, but it must not require a model server,
network access, Docker, OpenCode, or a usable tmux server.

## Hermetic suite

Run locally and in the primary CI workflow:

```sh
cargo test --locked --workspace --all-targets
```

Dependency changes additionally require both executable policy gates:

```sh
./scripts/check-dependency-policy
./scripts/check-supply-chain
```

The second command requires the pinned `cargo-deny 0.20.2`; accepted licenses,
sources, duplicate versions, and advisory exceptions are documented in
[`supply-chain-policy.md`](supply-chain-policy.md).

Ignored tests are not part of this suite. A test that starts depending on a
live service or platform capability must be moved to the corresponding suite;
it must not silently skip after partially exercising a workflow.

## Platform suite

Platform tests exercise real process facilities without contacting a model.
The initial suite covers the durable tmux capture and teardown path:

```sh
cargo test --locked -p corpus-core --lib \
  tui_raw_capture_is_durable_in_project_corpus -- \
  --ignored --exact --test-threads=1
```

CI installs tmux explicitly and runs this suite in its own `platform-tmux`
job. Platform tests should use unique session names and clean up processes and
temporary artifacts even when assertions fail.

## Live model suite

The dedicated `corpus-integration` harness owns all real-model scenarios,
including the complete curator orchestration campaign.
Every Qwen3.8 test uses only `qwen3.8:27b-mlx` and must acquire one shared
cross-process file lock before it loads or calls the model. The lease is
retained through cleanup and artifact capture. `--test-threads=1` is
additionally required but is not sufficient by itself because Cargo can run
separate test binaries concurrently. A non-MLX `CORPUS_QWEN38_MODEL` override
is rejected during preflight.

The suite pins and reports the exact Qwen3.8 MLX model identifier and digest. It
covers new-project creation, curator mission creation and launch, delegated
mission execution, findings and transcript persistence, mission completion,
exact-origin done notification back to the curator, curator synthesis, and
restart/recovery behavior. These tests run only on the prepared single-model
runner, never hosted CI.

Existing ignored live probes remain characterization tests until their
equivalent scenarios pass in `corpus-integration`:

- embedded chat probes require Ollama and the administration MCP binary;
- model discovery requires a real OpenCode installation;
- external plugin baseline coverage requires installed plugin sources and
  Docker.

Live tests must preserve their first-attempt logs and correlated run artifacts
on failure. Retrying may diagnose nondeterminism but may not replace the
original result.

The prepared runner currently pins `qwen3.8:27b-mlx` with Ollama digest
`5642e97495e1`. Run the serial smoke and embedded management scenarios with:

```sh
cargo test --locked -p corpus-integration --test model_qwen38 \
  configured_qwen38_model_runs_under_the_global_lease -- \
  --ignored --exact --nocapture --test-threads=1

cargo test --locked -p corpus-app --lib \
  injection_probe::live_ -- \
  --ignored --nocapture --test-threads=1

cargo build --locked -p corpus-mcp
cargo test --locked -p corpus-integration --test curator_system_qwen38 \
  curator_launches_children_serially_and_receives_exact_completions -- \
  --ignored --nocapture --test-threads=1
```

On 2026-08-25 the MLX runner passed real inference, operator project creation
and listing with the write-approval/read-no-approval contract, orchestrator
delegation through the project-manager specialist, and the depbot agent
creation regression in four tool calls with zero tool errors.

The final production coordinator campaign passed in 228.54 seconds. It
used real OpenCode/tmux processes to run curator → child one → exact completion
delivery and acknowledgement → coordinator restart and exact-session recovery
→ child two → exact completion delivery and acknowledgement. Launches were
gated on terminal-turn proof so only one MLX inference was active at a time.
