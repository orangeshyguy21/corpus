#!/usr/bin/env bash
# Oracle 001: both mints respond to /v1/info with a well-formed NUT-06 payload.
#
# Pre-flight, not a security invariant: an attack that "succeeds" because
# the mint was simply down is noise. Agents must not run against dead targets.
set -euo pipefail

for url in "$MINT_URL" "$MINT_URL_2"; do
    if ! body="$(curl -sf --max-time 5 "$url/v1/info")"; then
        echo "mint unreachable: $url"
        exit 2
    fi
    # NUT-06: name/motto/etc. are optional and may be absent entirely;
    # pubkey, version, and nuts are what identify a functional mint.
    if ! echo "$body" | jq -e 'has("pubkey") and has("version") and has("nuts")' >/dev/null; then
        echo "malformed /v1/info from $url"
        exit 1
    fi
done
