#!/usr/bin/env bash
# setup.sh — one-shot first-party setup for the cdk-regtest plugin.
#
#   1. Build the agent image + arena networks/gateway (arena.sh up)
#   2. Fetch the pinned source corpus into sources/ (manifest: sources.toml)
#   3. Populate tools/ with the compiled attack tools (build-tools.sh)
#   4. Run the doctor self-verification probes
#
# The regtest environment itself (`just regtest` in the cdk repo) is the
# target environment the plugin *depends* on; it is not owned here. Point
# CORPUS_CDK_REPO at the cdk checkout if it is not ~/Sites/cdk.
set -euo pipefail

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORPUS_ROOT="$(cd "$PLUGIN_DIR/../.." && pwd)"
CONFIG="$PLUGIN_DIR/config.toml"
MANIFEST="$CORPUS_ROOT/sources.toml"
SOURCES_DIR="$CORPUS_ROOT/sources"

# --- minimal flat-TOML readers (config.toml + sources.toml) --------------

config_get() { # config_get <section> <key>
    awk -v section="$1" -v key="$2" '
        function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
        /^\[[^]]+\]/ {
            cur = $0
            gsub(/\[|\]|[ \t]/, "", cur)
            next
        }
        cur == section {
            line = $0
            sub(/[ \t]*#.*$/, "", line)
            if (index(line, "=") == 0) next
            if (trim(substr(line, 1, index(line, "=") - 1)) == key) {
                val = trim(substr(line, index(line, "=") + 1))
                gsub(/^"|"$/, "", val)
                print val
                exit
            }
        }
    ' "$CONFIG"
}

manifest_get() { # manifest_get <name> <key> — reads [sources.<name>] blocks
    awk -v name="$1" -v key="$2" '
        function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
        /^\[sources\.[^]]+\]/ {
            cur = $0
            sub(/^\[sources\./, "", cur)
            gsub(/\]|[ \t]/, "", cur)
            next
        }
        cur == name {
            line = $0
            sub(/[ \t]*#.*$/, "", line)
            if (index(line, "=") == 0) next
            if (trim(substr(line, 1, index(line, "=") - 1)) == key) {
                val = trim(substr(line, index(line, "=") + 1))
                gsub(/^"|"$/, "", val)
                print val
                exit
            }
        }
    ' "$MANIFEST"
}

# --- pinned source cache ----------------------------------------------------

# fetch_one <name> <repo> <tag> <sha> — materialize sources/<name>/<sha>.
# Shallow-clones at the tag, SHA-verifies against the pin, and is
# idempotent: an up-to-date checkout is left alone.
fetch_one() {
    local name="$1" repo="$2" tag="$3" sha="$4"
    local dest="$SOURCES_DIR/$name/$sha" got
    local tmp="$dest.tmp.$$"
    if [ -d "$dest/.git" ] && [ "$(git -C "$dest" rev-parse HEAD 2>/dev/null || true)" = "$sha" ]; then
        echo "  sources/$name: already pinned ($sha)"
        return 0
    fi
    mkdir -p "$SOURCES_DIR/$name"
    rm -rf "$dest" "$tmp"
    echo "==> sources/$name: fetching $repo@$tag"
    # Operators often rewrite https://github.com -> git@github.com: in
    # their global gitconfig; run the clone with the global config scrubbed
    # so the URL is used as written (SSH may not be available in the caller's
    # shell), falling back to the default config if that fails.
    if ! GIT_CONFIG_GLOBAL="$CORPUS_NO_SCM" \
            git clone --quiet --depth 1 --branch "$tag" \
            "https://github.com/$repo.git" "$tmp" 2>/dev/null; then
        if ! git clone --quiet --depth 1 --branch "$tag" \
                "https://github.com/$repo.git" "$tmp" 2>/dev/null; then
            echo "!! sources/$name: clone failed (https://github.com/$repo.git)" >&2
            rm -rf "$tmp"
            return 1
        fi
    fi
    got="$(git -C "$tmp" rev-parse HEAD)"
    if [ "$got" != "$sha" ]; then
        # The pin may name a branch tip that has moved (e.g. nuts@main):
        # fetch the pinned sha itself (GitHub allows sha fetch) rather
        # than failing a fetch that would never match.
        if GIT_CONFIG_GLOBAL="$CORPUS_NO_SCM" git -C "$tmp" fetch --quiet --depth 1 origin "$sha" 2>/dev/null \
            && git -C "$tmp" checkout --quiet --detach "$sha" 2>/dev/null; then
            got="$(git -C "$tmp" rev-parse HEAD)"
        fi
    fi
    if [ "$got" != "$sha" ]; then
        echo "!! sources/$name: sha mismatch: expected $sha, got $got" >&2
        rm -rf "$tmp"
        return 1
    fi
    mv "$tmp" "$dest"
    echo "  sources/$name: pinned $tag @ $sha"
}

# fetch_pinned_sources — for each repo configured in config.toml [sources],
# look up its repo+tag in the manifest and fetch whatever pin is missing.
fetch_pinned_sources() {
    [ -f "$MANIFEST" ] || { echo "!! sources: manifest not found: $MANIFEST" >&2; exit 1; }
    CORPUS_NO_SCM="${CORPUS_NO_SCM:-$(mktemp)}"
    local name sha repo tag
    for name in cdk nuts; do
        sha="$(config_get sources "${name}_sha")"
        [ -n "$sha" ] || continue
        repo="$(manifest_get "$name" repo)"
        tag="$(manifest_get "$name" tag)"
        if [ -z "$repo" ] || [ -z "$tag" ]; then
            echo "!! sources/$name: manifest entry missing (repo/tag) in $MANIFEST" >&2
            exit 1
        fi
        fetch_one "$name" "$repo" "$tag" "$sha" || exit 1
    done
}

echo "==> arena: building image, networks, gateway"
bash "$PLUGIN_DIR/arena.sh" up

echo "==> sources: fetching pinned upstream source corpus"
fetch_pinned_sources

if [ ! -x "$PLUGIN_DIR/tools/cdk-cli" ]; then
    echo "==> tools: building cdk-cli (first time; cached afterwards)"
    bash "$PLUGIN_DIR/tools/build-tools.sh"
else
    echo "==> tools: cdk-cli already present"
fi

echo "==> doctor: self-verification probes"
bash "$PLUGIN_DIR/arena.sh" doctor

# Warm the regtest nix shell: oracle and faucet scripts re-exec into it
# when their LN tools are not on PATH, and a cold eval can take MINUTES —
# long enough to stall an agent run (the MCP call deadline bounds it, but
# every call timing out until the build finishes is a bad mission). A warm
# eval cache makes those re-execs sub-second. Best-effort only.
if command -v nix >/dev/null 2>&1; then
    repo="${CORPUS_CDK_REPO:-$HOME/Sites/cdk}"
    echo "==> nix: warming regtest shell ($repo#regtest)"
    nix develop "$repo#regtest" -c true || \
        echo "!! nix warm failed — first oracle/faucet calls may be slow" >&2
fi

echo "==> cdk-regtest plugin: setup complete"