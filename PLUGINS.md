# Corpus environment plugins

Corpus environment plugins are independently released, host-trusted bundles
that implement the newline-delimited JSON protocol `corpus.environment/1`.
Corpus owns the vocabulary and durable session records; a plugin owns its
Docker topology, setup, sandbox, faucet, wallet integration, and oracles.

Production discovery reads only explicitly selected immutable installs under
`$CORPUS_HOME/plugins` (default `~/.corpus/plugins`). A source checkout is not
discovered implicitly. `CORPUS_PLUGINS_DIR` is a complete catalog override for
development and tests.

## Install and operate

For the normal path, choose a supported environment in the desktop app or use
its catalog id from the CLI:

```bash
corpus plugin install nutshell-regtest
corpus plugin setup nutshell-regtest
corpus plugin doctor nutshell-regtest
corpus plugin status nutshell-regtest
```

The built-in [`plugin-catalog.toml`](plugin-catalog.toml) pins each public
release URL and SHA-256 digest. `install` downloads only a catalog entry,
enforces archive size/path/type limits, verifies the digest and manifest
identity, copies the bundle into a read-only version directory, and selects it.
Setup is idempotent and may fetch pinned sources and build images. Doctor is a
read-only verification; status is the fast readiness view. Stop refuses while
the plugin has live mission leases.

Plugin development keeps an explicit local path:

```bash
corpus plugin install-local /path/to/unpacked-plugin-bundle
```

Session close is a verified postcondition: a plugin must report failure when
session containers or networks cannot be removed, leave the session retryable,
and report success only after proving those resources absent. Operation status
must invalidate a legacy successful close when resources or plugin session
state still show live.

Installing a newer bundle preserves prior versions. Roll back explicitly:

```bash
corpus plugin select nutshell-regtest 0.2.0
corpus plugin doctor nutshell-regtest
```

Selection changes future sessions only. Existing run evidence retains the
exact plugin version, bundle digest, environment lock, image digest, and source
SHAs with which it was produced. Offline relaunch works when the selected
bundle, its prepared environment, and the required source SHAs remain cached.

The current built-in release locks are:

| Plugin | Tag | Release archive SHA-256 |
|---|---|---|
| `cdk-regtest` | `corpus-plugin-cdk@v0.4.8` | `c16f0c7f36787fb2f3d73e769c42a851e0dfa817398e9718dd51b3919c86ffd2` |
| `nutshell-regtest` | `corpus-plugin-nutshell@v0.4.3` | `f45670cdb0d09d0f11125f5e228554fdcd9de47885c510cd131a4e7e0179bcf0` |

Corpus CI downloads both assets, verifies both the attached checksum and this
independent lock, installs them through the operator path, negotiates v1, and
runs the language-neutral protocol suite.

## Author a plugin

A bundle contains `plugin.toml` and a relative executable. The minimum v1
manifest identifies an immutable version and protocol:

```toml
manifest_version = 1
id = "example-regtest"
version = "0.1.0"
protocol = "corpus.environment/1"
exec = "plugin"
capabilities = ["sessions", "sandbox.exec", "lifecycle.setup"]

[[sources]]
id = "target"
repo = "owner/target"
default_rev = "v1.0.0"
default_sha = "0123456789abcdef0123456789abcdef01234567"
mount = "/opt/src/target"
```

The executable reads one JSON request per line from stdin and writes one JSON
reply per line to stdout. Diagnostics go to stderr. It must negotiate `hello`,
declare only the supported capability vocabulary, use the Corpus-provided
absolute plugin/state/cache/source paths, and treat `session_id` and
idempotency keys as durable identities. It must never infer the Corpus checkout
or write inside its installed bundle.

Use the fixtures in `crates/corpus-core/tests/v1-echo-plugin` and the protocol
tests in `crates/corpus-core/tests/protocol.rs` as the executable contract.
Before release, run the plugin repository's conformance and Docker smoke jobs,
package with `git archive`, publish a signed version tag, attach the archive and
checksum, and then update Corpus's compatibility lock. Protocol additions land
in Corpus fixtures first; plugins adopt them before Corpus requires them.

Plugins are trusted host code. Review and checksum a bundle before installing
it. The execution sandbox constrains attacker code, not the plugin process
that creates that sandbox.
