#!/usr/bin/env bash
# Oracle 020: conservation of value.
#
# Invariant: a mint's outstanding liabilities — the summed value of proofs
# in state UNSPENT, PENDING, or RESERVED — must never exceed the total
# balance of the Lightning node backing that mint. A violation means value
# exists that was never paid for: unauthorized minting, a double-spend that
# stuck, a quote settled twice, or fee/rounding exploited upward.
#
# Runs host-side against the live regtest environment (just regtest).
# Needs lightning-cli and lncli (plus sqlite3, jq). Resolution order per
# tool: the CORPUS_LIGHTNING_CLI / CORPUS_LNCLI / CORPUS_SQLITE3 / CORPUS_JQ
# absolute-path override, else PATH. A missing tool yields a fast
# `inconclusive` — deliberately NOT a `nix develop .#regtest` re-exec:
# instantiating that devshell rebuilds the regtest start-scripts on every
# call and blows past the harness oracle timeout (the plugin then killed the
# whole process tree). Point the overrides at real binaries instead.
set -uo pipefail

ENV_FILE="/tmp/cdk_regtest_env"
if [ ! -f "$ENV_FILE" ]; then
    echo "regtest env state not found (just regtest first)"
    exit 2
fi
# shellcheck disable=SC1090
source "$ENV_FILE"
if [ -z "${CDK_ITESTS_DIR:-}" ] || [ ! -d "$CDK_ITESTS_DIR" ]; then
    echo "CDK_ITESTS_DIR missing"
    exit 2
fi
# Resolve a tool to an absolute path: an explicit override wins, else PATH.
# No `nix develop` re-exec by design — see the header note.
resolve_tool() { # resolve_tool <override> <command-name>
    local override="$1" name="$2" path
    if [ -n "$override" ]; then
        [ -x "$override" ] && { printf '%s\n' "$override"; return 0; }
        echo "override for $name is not executable: $override" >&2
        return 1
    fi
    path="$(command -v "$name" 2>/dev/null)" && { printf '%s\n' "$path"; return 0; }
    return 1
}

missing=""
SQLITE3="$(resolve_tool "${CORPUS_SQLITE3:-}" sqlite3)"                   || missing="$missing sqlite3"
JQ="$(resolve_tool "${CORPUS_JQ:-}" jq)"                                  || missing="$missing jq"
LIGHTNING_CLI="$(resolve_tool "${CORPUS_LIGHTNING_CLI:-}" lightning-cli)" || missing="$missing lightning-cli"
LNCLI="$(resolve_tool "${CORPUS_LNCLI:-}" lncli)"                         || missing="$missing lncli"
if [ -n "$missing" ]; then
    echo "conservation oracle cannot run — tools not resolvable:$missing"
    echo "fix: set CORPUS_LIGHTNING_CLI / CORPUS_LNCLI (and CORPUS_SQLITE3 / CORPUS_JQ if needed)"
    echo "     to absolute paths, or run from a shell that already has them on PATH."
    exit 2
fi

# In-flight operations (melt paid, change pending) can transiently skew the
# sums by a few sats; only flag discrepancies beyond this tolerance.
TOL_SATS=1000

# Outstanding liabilities in sat-denominated keysets (other units skipped).
liabilities() { # liabilities <mint db>
    "$SQLITE3" "$1" "SELECT COALESCE(SUM(p.amount), 0)
                  FROM proof p JOIN keyset k ON p.keyset_id = k.id
                  WHERE k.unit = 'sat'
                    AND p.state IN ('UNSPENT', 'PENDING', 'RESERVED');"
}

violated=0

# --- CLN mint (8085) backed by CLN node "one" ---
cln_db="$CDK_ITESTS_DIR/cln_mint/cdk-mintd.sqlite"
cln_rpc="$CDK_ITESTS_DIR/cln/one/regtest/lightning-rpc"
if [ ! -f "$cln_db" ] || [ ! -S "$cln_rpc" ]; then
    echo "cln mint artifacts missing"
    exit 2
fi
cln_msat="$("$LIGHTNING_CLI" --rpc-file="$cln_rpc" listfunds | "$JQ" '
    ([.outputs[]?.amount_msat, .channels[]?.our_amount_msat]
     | map(tostring | sub("msat$"; "") | tonumber) | add) // 0')"
cln_balance=$((cln_msat / 1000))
cln_liab="$(liabilities "$cln_db")"
echo "cln mint:  liabilities=${cln_liab} sat  node_balance=${cln_balance} sat"
if [ "$cln_liab" -gt $((cln_balance + TOL_SATS)) ]; then
    echo "VIOLATION: cln mint liabilities exceed backing by $((cln_liab - cln_balance)) sat"
    violated=1
fi

# --- LND mint (8087) backed by LND node "two" ---
lnd_db="$CDK_ITESTS_DIR/lnd_mint/cdk-mintd.sqlite"
lnd_dir="$CDK_ITESTS_DIR/lnd/two"
if [ ! -f "$lnd_db" ] || [ ! -f "$lnd_dir/tls.cert" ]; then
    echo "lnd mint artifacts missing"
    exit 2
fi
lnd_balance="$({
    "$LNCLI" --rpcserver=localhost:10010 \
          --tlscertpath="$lnd_dir/tls.cert" \
          --macaroonpath="$lnd_dir/data/chain/bitcoin/regtest/admin.macaroon" \
          walletbalance | "$JQ" '.total_balance | tonumber'
    "$LNCLI" --rpcserver=localhost:10010 \
          --tlscertpath="$lnd_dir/tls.cert" \
          --macaroonpath="$lnd_dir/data/chain/bitcoin/regtest/admin.macaroon" \
          channelbalance | "$JQ" '(.balance // .local_balance.sat // 0) | tonumber'
} | awk '{s += $1} END {print s}')"
lnd_liab="$(liabilities "$lnd_db")"
echo "lnd mint:  liabilities=${lnd_liab} sat  node_balance=${lnd_balance} sat"
if [ "$lnd_liab" -gt $((lnd_balance + TOL_SATS)) ]; then
    echo "VIOLATION: lnd mint liabilities exceed backing by $((lnd_liab - lnd_balance)) sat"
    violated=1
fi

exit "$violated"
