#!/usr/bin/env bash
# corpus plugin: nutshell-fake — Nutshell (Python Cashu mint) with the
# FakeWallet Lightning backend, built from a PINNED local checkout.
#
# FakeWallet pays every invoice itself, so agents can fund wallets for
# free — no faucet needed. That makes Nutshell the cheapest target for
# funded attack missions (double-spend races, melt abuse).
set -uo pipefail

NUTSHELL_DIR="${NUTSHELL_DIR:-$HOME/Sites/nutshell}"
IMAGE="${NUTSHELL_IMAGE:-corpus-nutshell:local}"
CONTAINER="${NUTSHELL_CONTAINER:-corpus-nutshell}"
PORT="${NUTSHELL_PORT:-3338}"
ORACLES_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/oracles"

reply_ok()   { printf '{"id":%s,"ok":true,"result":%s}\n' "$1" "$2"; }
reply_err()  { printf '{"id":%s,"ok":false,"error":%s}\n' "$1" "$(jq -Rn --arg e "$2" '$e')"; }

running() { [ "$(docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null)" = "true" ]; }
info_ok() { curl -sf --max-time 3 "http://127.0.0.1:$PORT/v1/info" >/dev/null 2>&1; }
rev()     { git -C "$NUTSHELL_DIR" rev-parse --short HEAD 2>/dev/null || echo "unknown"; }

sanitized_name() {
    case "$1" in *[!a-zA-Z0-9._-]* | "" | *..*) return 1 ;; esac
    printf '%s' "$1"
}

handle() {
    local id="$1" method="$2" params="$3"

    case "$method" in
        probe)
            local ready=false notes
            if ! docker info >/dev/null 2>&1; then
                notes="docker daemon not running"
            elif running && info_ok; then
                ready=true
                notes="nutshell up on :$PORT (rev $(rev))"
            elif [ ! -d "$NUTSHELL_DIR" ]; then
                notes="nutshell checkout not found at $NUTSHELL_DIR (set NUTSHELL_DIR)"
            elif docker image inspect "$IMAGE" >/dev/null 2>&1; then
                notes="image $IMAGE built; not running (call up)"
            else
                notes="no image; call up to build from pinned checkout (rev $(rev), slow first time)"
            fi
            reply_ok "$id" "$(jq -nc --argjson ready "$ready" --arg notes "$notes" \
                '{ready:$ready, notes:$notes}')"
            ;;
        up)
            [ -d "$NUTSHELL_DIR" ] || { reply_err "$id" "no checkout at $NUTSHELL_DIR"; return; }
            if running && info_ok; then
                reply_ok "$id" "$(jq -nc --arg port "$PORT" '{notes:"already running", port:$port}')"
                return
            fi
            if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
                echo "building $IMAGE from $NUTSHELL_DIR (rev $(rev))..." >&2
                if ! out="$(docker build -q -t "$IMAGE" "$NUTSHELL_DIR" 2>&1)"; then
                    reply_err "$id" "docker build failed: $out"; return
                fi
            fi
            docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
            if ! docker run -d --name "$CONTAINER" -p "$PORT:3338" \
                -e MINT_LIGHTNING_BACKEND=FakeWallet \
                -e MINT_LISTEN_HOST=0.0.0.0 \
                -e MINT_LISTEN_PORT=3338 \
                -e MINT_PRIVATE_KEY=corpus-test-key \
                -e MINT_INPUT_FEE_PPK=100 \
                "$IMAGE" poetry run mint >/dev/null; then
                reply_err "$id" "docker run failed"; return
            fi
            for _ in $(seq 1 45); do info_ok && break; sleep 1; done
            if info_ok; then
                reply_ok "$id" "$(jq -nc --arg port "$PORT" --arg rev "$(rev)" \
                    '{notes:"nutshell up", port:$port, rev:$rev}')"
            else
                logs="$(docker logs "$CONTAINER" 2>&1 | tail -5)"
                reply_err "$id" "nutshell did not become ready: $logs"
            fi
            ;;
        down)
            docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
            reply_ok "$id" "$(jq -nc '{notes:"nutshell down"}')"
            ;;
        targets)
            reply_ok "$id" "$(jq -nc --arg url "http://127.0.0.1:$PORT" '[ $url ]')"
            ;;
        oracles)
            local items="[]"
            for f in "$ORACLES_DIR"/[0-9]*.sh; do
                [ -f "$f" ] || continue
                local name desc
                name="$(basename "$f" .sh)"
                desc="$(grep -m1 '^# Oracle' "$f" | sed 's/^# //')"
                items="$(jq -nc --argjson items "$items" --arg name "$name" --arg desc "$desc" \
                    '$items + [{name:$name, description:$desc}]')"
            done
            reply_ok "$id" "$items"
            ;;
        call_oracle)
            local name
            name="$(jq -r '.name // ""' <<<"$params")"
            name="$(sanitized_name "$name")" || { reply_err "$id" "bad oracle name"; return; }
            local script="$ORACLES_DIR/$name.sh"
            [ -f "$script" ] || { reply_err "$id" "unknown oracle: $name"; return; }
            local out rc verdict
            out="$(MINT_URL="http://127.0.0.1:$PORT" bash "$script" 2>&1)" && rc=0 || rc=$?
            case "$rc" in
                0) verdict="hold" ;;
                1) verdict="violated" ;;
                *) verdict="inconclusive" ;;
            esac
            reply_ok "$id" "$(jq -nc --arg verdict "$verdict" --arg log "$out" \
                '{verdict:$verdict, log:$log}')"
            ;;
        *)
            reply_err "$id" "unknown method: $method"
            ;;
    esac
}

while IFS= read -r line; do
    id="$(jq -r '.id // 0' <<<"$line" 2>/dev/null)" || id=0
    method="$(jq -r '.method // ""' <<<"$line" 2>/dev/null)" || method=""
    params="$(jq -c '.params // {}' <<<"$line" 2>/dev/null)" || params="{}"
    handle "$id" "$method" "$params"
done
