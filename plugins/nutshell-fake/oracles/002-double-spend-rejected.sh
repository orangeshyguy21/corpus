#!/usr/bin/env bash
# Oracle 002: a spent proof can never be redeemed again (NUT-03/07).
#
# Functional invariant, free to check against FakeWallet: mint, send,
# receive once (must succeed), receive the SAME token again into a fresh
# wallet (MUST be rejected). A successful second redemption is a
# demonstrated double-spend — the core ecash invariant broken.
#
# Runs cdk-cli inside the shared agent container (Linux binary); needs
# CORPUS_TOOLS_DIR with a cdk-cli build and CORPUS_AGENT_IMAGE.
set -uo pipefail

: "${MINT_URL:?}"
TOOLS_DIR="${CORPUS_TOOLS_DIR:-$HOME/Sites/corpus/plugins/cdk-regtest/tools}"
IMAGE="${CORPUS_AGENT_IMAGE:-corpus-agent:local}"

if [ ! -x "$TOOLS_DIR/cdk-cli" ]; then
    echo "cdk-cli not found in $TOOLS_DIR (build: plugins/cdk-regtest/tools/build-tools.sh)"
    exit 2
fi
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "agent image $IMAGE not available"
    exit 2
fi

# Inside the container the host is reached via host.docker.internal.
CONTAINER_URL="${MINT_URL/127.0.0.1/host.docker.internal}"
CONTAINER_URL="${CONTAINER_URL/localhost/host.docker.internal}"

# NB: -i is required — bash -s reads the script from stdin.
out="$(docker run --rm -i \
    --add-host host.docker.internal:host-gateway \
    -e CONTAINER_URL="$CONTAINER_URL" \
    -v "$TOOLS_DIR:/opt/tools:ro" \
    "$IMAGE" bash -s <<'SCRIPT'
set -uo pipefail
C="/opt/tools/cdk-cli -n"
URL="$CONTAINER_URL"

# Wallet A: mint (FakeWallet auto-pays) and send 32 sat.
rm -rf /tmp/wA /tmp/wB
$C -w /tmp/wA mint "$URL" 64 >/dev/null 2>&1 || { echo "mint failed"; exit 2; }
echo "wallet A balance: $($C -w /tmp/wA balance 2>/dev/null | head -1)"
token="$($C -w /tmp/wA send -a 32 --mint-url "$URL" 2>/dev/null | grep -E '^cashu' | tail -1)"
[ -n "$token" ] || { echo "send produced no token"; exit 2; }

# First redemption: must succeed.
if ! $C -w /tmp/wA receive "$token" >/dev/null 2>&1; then
    echo "first redemption failed — mint is broken, not secure"
    exit 2
fi

# Second redemption into a FRESH wallet: MUST be rejected.
if $C -w /tmp/wB receive "$token" --allow-untrusted >/dev/null 2>&1; then
    echo "VIOLATION: the same token was redeemed twice (double-spend)"
    exit 1
fi
echo "second redemption rejected — invariant holds"
SCRIPT
)"

# Fail closed on evidence markers, not exit codes: a container that ran
# nothing (broken stdin, missing binary) must never look like "hold".
printf '%s\n' "$out"
if grep -q "VIOLATION" <<<"$out"; then exit 1; fi
if grep -q "invariant holds" <<<"$out"; then exit 0; fi
echo "no conclusive evidence in output"
exit 2
