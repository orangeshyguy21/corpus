#!/usr/bin/env bash
# faucet.sh — regtest Lightning faucet for cdk-regtest agents. HOST-SIDE ONLY.
#
# The plugin invokes this on behalf of the sandboxed agent, which has no
# Lightning access of its own. Two operations:
#
#   faucet.sh pay <bolt11>                Pay an invoice (mint quotes etc.)
#   faucet.sh invoice <amount_sat> [memo] Create an invoice (melt destination)
#   faucet.sh balance                     Faucet wallet balance
#
# Safety rules, enforced here:
#   - REGTEST ONLY. The invoice is decoded and rejected unless its currency
#     is bcrt. No amount of prompt injection can turn this into a mainnet
#     payment: there are no mainnet keys on this path.
#   - Per-payment cap (CORPUS_FAUCET_MAX_SATS, default 100_000 sat).
#   - Amountless invoices are rejected (unbounded payment).
#
# The faucet wallet is CLN node "two" — deliberately NOT a mint backend
# (cln one backs the CLN mint, lnd two backs the LND mint), so funded flows
# exercise real cross-node Lightning paths.
#
# Absorbed from cdk/bin/faucet.sh (2026-08-10); env vars renamed
# VUL_LAB_* -> CORPUS_*. The /tmp/cdk_regtest_env contract is unchanged:
# it belongs to the regtest environment this plugin targets.
set -euo pipefail

ENV_FILE="/tmp/cdk_regtest_env"
MAX_SATS="${CORPUS_FAUCET_MAX_SATS:-100000}"

die() { echo "faucet: error: $*" >&2; exit 1; }

# LN tooling lives in the regtest nix shell; re-exec into it once if needed.
if ! command -v lightning-cli >/dev/null 2>&1; then
    if [ -z "${CORPUS_NIX_REEXEC:-}" ] && command -v nix >/dev/null 2>&1; then
        # The regtest environment is owned by the cdk repo; path is a
        # plugin dependency, configurable so corpus stays self-contained
        # relative to an arbitrary checkout location.
        repo="${CORPUS_CDK_REPO:-$HOME/Sites/cdk}"
        # Note: nix noise goes to stderr; consumers parse stdout markers
        # (PAID_SATS=..., lnbcrt...), so it is harmless.
        CORPUS_NIX_REEXEC=1 exec nix develop "$repo#regtest" -c bash "$0" "$@"
    fi
    die "lightning-cli not on PATH (nix develop .#regtest)"
fi

[ -f "$ENV_FILE" ] || die "regtest env not found (just regtest first)"
# shellcheck disable=SC1090
source "$ENV_FILE"
[ -n "${CDK_ITESTS_DIR:-}" ] && [ -d "$CDK_ITESTS_DIR" ] || die "CDK_ITESTS_DIR missing"

CLN_RPC="$CDK_ITESTS_DIR/cln/two/regtest/lightning-rpc"
[ -S "$CLN_RPC" ] || die "faucet node (cln two) not available"

cln() { lightning-cli --rpc-file="$CLN_RPC" "$@"; }

cmd_pay() {
    local invoice="${1:-}"
    [ -n "$invoice" ] || die "usage: faucet.sh pay <bolt11>"
    case "$invoice" in
        lnbcrt*) ;;
        *) die "refused: not a regtest (lnbcrt) invoice" ;;
    esac

    local decoded currency amount_msat
    decoded="$(cln decode "$invoice")" || die "undecodable invoice"
    currency="$(echo "$decoded" | jq -r '.currency // empty')"
    [ "$currency" = "bcrt" ] || die "refused: invoice currency is '$currency', not bcrt"
    amount_msat="$(echo "$decoded" | jq -r '.amount_msat // empty')"
    [ -n "$amount_msat" ] || die "refused: amountless invoice (unbounded payment)"

    local amount_sats
    amount_sats=$(( ${amount_msat%msat} / 1000 ))
    [ "$amount_sats" -le "$MAX_SATS" ] || \
        die "refused: $amount_sats sat exceeds per-payment cap ($MAX_SATS sat)"

    echo "paying $amount_sats sat..." >&2
    local result status
    # NB: `timeout` cannot wrap the cln() shell function — call the binary.
    result="$(timeout 60 lightning-cli --rpc-file="$CLN_RPC" pay "$invoice")" \
        || die "payment failed or timed out"
    status="$(echo "$result" | jq -r '.status // empty')"
    [ "$status" = "complete" ] || die "payment not complete: $status"
    # Machine-readable line: the plugin parses this for session budgeting.
    echo "PAID_SATS=$amount_sats"
}

cmd_invoice() {
    local amount_sats="${1:-}"
    [ -n "$amount_sats" ] || die "usage: faucet.sh invoice <amount_sat> [memo]"
    case "$amount_sats" in
        *[!0-9]* | "") die "amount must be a positive integer (sat)" ;;
    esac
    [ "$amount_sats" -gt 0 ] || die "amount must be > 0"
    [ "$amount_sats" -le "$MAX_SATS" ] || \
        die "refused: $amount_sats sat exceeds per-invoice cap ($MAX_SATS sat)"

    local label memo
    label="corpus-$(date +%s)-$RANDOM"
    memo="${2:-corpus faucet}"
    cln invoice "$((amount_sats * 1000))msat" "$label" "$memo" | jq -r '.bolt11'
}

cmd_balance() {
    cln listfunds | jq '[.outputs[]?.amount_msat, .channels[]?.our_amount_msat]
        | map(tostring | sub("msat$"; "") | tonumber) | add / 1000 | floor'
}

main() {
    local cmd="${1:-}"
    shift || true
    case "$cmd" in
        pay) cmd_pay "$@" ;;
        invoice) cmd_invoice "$@" ;;
        balance) cmd_balance "$@" ;;
        *) die "usage: faucet.sh pay <bolt11> | invoice <amount_sat> [memo] | balance" ;;
    esac
}

main "$@"