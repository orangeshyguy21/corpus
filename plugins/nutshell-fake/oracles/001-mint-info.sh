#!/usr/bin/env bash
# Oracle 001: the Nutshell mint responds to /v1/info with a well-formed
# NUT-06 payload. Pre-flight: attacks against a dead mint are noise.
set -euo pipefail

: "${MINT_URL:?}"
if ! body="$(curl -sf --max-time 5 "$MINT_URL/v1/info")"; then
    echo "mint unreachable: $MINT_URL"
    exit 2
fi
# NUT-06: name/motto are optional; pubkey, version, nuts identify a mint.
if ! echo "$body" | jq -e 'has("pubkey") and has("version") and has("nuts")' >/dev/null; then
    echo "malformed /v1/info from $MINT_URL"
    exit 1
fi
