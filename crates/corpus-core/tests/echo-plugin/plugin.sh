#!/usr/bin/env bash
# echo-plugin: a fake plugin for corpus-core protocol tests. It echoes the
# command back for sandbox_exec, returns a fixed oracle verdict/log, and
# returns canned targets/tools/faucet responses — no docker, no host side
# effects. The test asserts the CLIENT side of the protocol, not the
# plugin's environment.
set -uo pipefail

handle() {
    local id="$1" method="$2" params="$3"
    case "$method" in
        probe)
            printf '{"id":%s,"ok":true,"result":{"ready":true,"notes":"echo up"}}\n' "$id"
            ;;
        targets)
            printf '{"id":%s,"ok":true,"result":["http://echo-gw:8085","http://echo-gw:8087"]}\n' "$id"
            ;;
        tools)
            printf '{"id":%s,"ok":true,"result":["/opt/tools/cdk-cli"]}\n' "$id"
            ;;
        sandbox_exec)
            local command sources
            command="$(jq -r '.command // ""' <<<"$params")"
            # Reflect the source pins only when the caller sent them.
            sources="$(jq -rnc --argjson p "$params" 'if $p.sources then ":" + ($p.sources|tostring) else "" end')"
            jq -nc --argjson id "$id" --arg output "echo-container:${command}${sources}" \
                '{id:$id, ok:true, result:{output:$output, exit_code:0}}'
            ;;
        sources)
            # Mirror the real plugin: params.sources overrides the sha,
            # else the canned default pin is reported.
            jq -rnc --argjson id "$id" --argjson p "$params" '
                ($p.sources // {}) as $over |
                {id:$id, ok:true, result:[
                    {name:"cdk", repo:"cashubtc/cdk", tag:"v0.17.0",
                     sha:($over.cdk // "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                     mount:"/opt/src/cdk"}
                ]}'
            ;;
        oracles)
            printf '{"id":%s,"ok":true,"result":[{"name":"001-echo","description":"echo oracle"}]}\n' "$id"
            ;;
        call_oracle)
            printf '{"id":%s,"ok":true,"result":{"verdict":"violated","log":"echo oracle log"}}\n' "$id"
            ;;
        faucet)
            local op
            op="$(jq -r '.op // ""' <<<"$params")"
            case "$op" in
                pay)    printf '{"id":%s,"ok":true,"result":{"text":"paid 42 sat","paid_sats":42}}\n' "$id" ;;
                invoice) printf '{"id":%s,"ok":true,"result":{"text":"lnbcrt1echo","paid_sats":null}}\n' "$id" ;;
                balance) printf '{"id":%s,"ok":true,"result":{"text":"42","paid_sats":null}}\n' "$id" ;;
                *)      printf '{"id":%s,"ok":false,"error":"unknown op"}\n' "$id" ;;
            esac
            ;;
        *)
            printf '{"id":%s,"ok":false,"error":"unknown method: %s"}\n' "$id" "$method"
            ;;
    esac
}

while IFS= read -r line; do
    id="$(jq -r '.id // 0' <<<"$line" 2>/dev/null)" || id=0
    method="$(jq -r '.method // ""' <<<"$line" 2>/dev/null)" || method=""
    params="$(jq -c '.params // {}' <<<"$line" 2>/dev/null)" || params="{}"
    handle "$id" "$method" "$params"
done