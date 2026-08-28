<p align="center">
  <img src="crates/corpus-app/assets/logo.png" alt="Corpus" width="536">
</p>

<p align="center">
  <strong>Local-first, verification-driven vulnerability research.</strong>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#command-line-quick-start">CLI</a> ·
  <a href="#contributing">Contributing</a> ·
  <a href="docs/operator-guide.md">Operator guide</a>
</p>

> [!WARNING]
> Corpus is pre-alpha software. Use it only against systems you own or have
> explicit written permission to test. The intended target is a local,
> disposable environment—not production.

## What is Corpus?

Corpus is a local-first platform for autonomous vulnerability research. It
runs AI research agents against source code pinned to exact revisions, executes
proofs of concept inside plugin-provided sandboxes, checks results with
programmatic oracles, and stores attributable evidence in a private local
corpus.

## Quick start

### Prerequisites

| Requirement | Version / purpose | Required? |
|---|---|---|
| Git | Clone the repository and pinned target sources | Yes |
| Rust via `rustup` | Toolchain `1.97.1` is pinned in `rust-toolchain.toml` | Yes |
| OpenCode | `1.18.18` or a newer `1.18.x` patch; runs research agents | Yes |
| Model provider | Any provider configured in OpenCode; launches use an explicit `provider/model` | Yes |
| Docker | Runs the current `cdk-regtest` and `nutshell-regtest` environments | Yes for bundled plugins |
| tmux | `3.2a` or newer; enables attachable desktop sessions | Recommended |

OpenCode must be available on `PATH` or installed at
`~/.opencode/bin/opencode`. Docker must be running before plugin setup.

### Install from source

```sh
git clone https://github.com/orangeshyguy21/corpus.git
cd corpus

rustup show
cargo build --locked --workspace
```

The build produces the desktop app, the `corpus` CLI, and its companion MCP
servers in `target/debug/`.

### Start the desktop app

Launch Corpus from the repository root:

```sh
./target/debug/corpus-app
```

The app guides the rest of the setup. Install a supported environment when
prompted, then create a project and choose its environment plugin. Open the
project's environment controls and run **Setup**, followed by **Doctor**, to
prepare its pinned sources and Docker services and confirm that it is ready.

Next, add an agent to the project and choose the role that matches its job.
Create a mission, select an explicit model, and use **Launch** to start the
run. Corpus opens the mission in its integrated terminal and keeps the
transcript, findings, and other evidence attached to that project.

Use the sidebar to move between projects, agents, and missions. The project
view is where you inspect environment health and findings; the mission view is
where you launch, resume, and follow active work.

Corpus stores operator-owned data under `~/.corpus` by default. Set
`CORPUS_HOME` before first use to place it elsewhere.

## Command-line quick start

Prefer a terminal? Add the freshly built binaries to the current shell, then
confirm the model identifiers available through OpenCode:

```sh
export PATH="$PWD/target/debug:$PATH"
opencode models
```

Install and prepare an environment, create a project and agent, then launch a
mission:

```sh
corpus plugin install cdk-regtest
corpus plugin setup cdk-regtest
corpus plugin doctor cdk-regtest

corpus project new example --name "Example" --plugin cdk-regtest
corpus agent new example tester --role tester

CORPUS_PROJECT=example corpus run tester \
  --model <provider/model> \
  "Investigate the authorized target and verify any findings"
```

The project scope is always explicit—Corpus will not silently choose one for
a run. Replace `<provider/model>` with an identifier from `opencode models`.
To use the Nutshell environment, replace `cdk-regtest` with
`nutshell-regtest`.

That is enough to get started. For the full command catalog, roles, mission
lifecycle, cleanup, audit logs, and configuration overrides, continue to the
[operator guide](docs/operator-guide.md). If a launch, plugin, or model is not
ready, see [the troubleshooting guide](docs/troubleshooting.md).

## Contributing

### Development setup

Fork and clone the repository, then build the complete workspace with the
locked dependency graph:

```sh
rustup show
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
```

The repository's pinned toolchain installs `rustfmt`, Clippy, and
`rust-analyzer`. The default test suite is hermetic: it does not require a
model server, network access, Docker, OpenCode, or tmux.

### Before opening a pull request

Run the checks relevant to your change:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
./scripts/check-dependency-policy
git diff --check
```

Dependency changes must also pass the supply-chain gate:

```sh
cargo install --locked cargo-deny --version 0.20.2
./scripts/check-supply-chain
```

Keep the following contribution rules in mind:

- Preserve project scope, role ceilings, confirmation gates, and filesystem
  confinement; review [the threat model](docs/threat-model.md) for security
  invariants.
- Add characterization tests before changing unclear behavior.
- Keep normal tests hermetic. Platform, plugin, and live-model tests have
  separate, explicitly invoked suites.
- Update architecture and dependency-policy documentation when crate ownership
  or workspace edges change.
- Do not edit production plugin implementations in this repository; plugins
  are independently released and tested against Corpus's protocol fixtures.
- Never relax a security assertion merely to make a test pass.

### Workspace map

| Path | Responsibility |
|---|---|
| `crates/corpus-app` | Desktop operator application |
| `crates/corpus-cli` | Headless `corpus` command |
| `crates/corpus-core` | Plugins, sources, launches, processes, and compatibility facade |
| `crates/corpus-store` | Durable filesystem data model |
| `crates/corpus-observe` | Read-only host projections |
| `crates/corpus-admin*` | Host-side administration policy and MCP server |
| `crates/corpus-mcp` | Project-scoped research MCP server |
| `crates/corpus-integration` | Hermetic and live end-to-end scenarios |
| `benchmarks` | Model registry and forensic fixtures |
| `docs` | Architecture, operations, testing, and security documentation |

For the complete contributor contract, read [AGENTS.md](AGENTS.md) and the
[testing guide](docs/testing.md).

## Documentation

| Guide | Use it for |
|---|---|
| [Operator guide](docs/operator-guide.md) | Routine setup, projects, missions, cleanup, and diagnostics |
| [Plugin guide](PLUGINS.md) | Plugin installation, rollback, lifecycle, and authoring |
| [Architecture](docs/architecture.md) | Runtime boundaries, crate ownership, and data flow |
| [Threat model](docs/threat-model.md) | Trust zones and enforced security invariants |
| [Testing](docs/testing.md) | Hermetic, platform, plugin, and live-model suites |
| [Troubleshooting](docs/troubleshooting.md) | Symptom-first recovery and evidence collection |

## License

See [LICENSE](LICENSE).
