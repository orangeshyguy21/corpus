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

Download a tagged archive and its checksum, verify it, unpack it, then install
the unpacked directory:

```bash
shasum -a 256 -c corpus-plugin-nutshell-0.3.0.tar.gz.sha256
tar -xzf corpus-plugin-nutshell-0.3.0.tar.gz
corpus plugin install corpus-plugin-nutshell-0.3.0
corpus plugin setup nutshell-regtest
corpus plugin doctor nutshell-regtest
corpus plugin status nutshell-regtest
```

`install` copies the bundle into a read-only version directory and selects it.
Setup is idempotent and may fetch pinned sources and build images. Doctor is a
read-only verification; status is the fast readiness view. Stop refuses while
the plugin has live mission leases.

Installing a newer bundle preserves prior versions. Roll back explicitly:

```bash
corpus plugin select nutshell-regtest 0.2.0
corpus plugin doctor nutshell-regtest
```

Selection changes future sessions only. Existing run evidence retains the
exact plugin version, bundle digest, environment lock, image digest, and source
SHAs with which it was produced. Offline relaunch works when the selected
bundle, its prepared environment, and the required source SHAs remain cached.

The current private release locks are:

| Plugin | Tag | Release archive SHA-256 |
|---|---|---|
| `cdk-regtest` | `corpus-plugin-cdk@v0.4.5` | `8bb5fb68cdad18d6688e195b4d02d291e553f2dfd470c0fec68edcd52c25d2ee` |
| `nutshell-regtest` | `corpus-plugin-nutshell@v0.4.1` | `fb8fa891634f9c40c8b3bbeb1a1b631a5433705a1e7c78822ed4a8479e27f9aa` |

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
