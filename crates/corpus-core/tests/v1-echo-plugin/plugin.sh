#!/usr/bin/env bash
set -uo pipefail

while IFS= read -r line; do
    id="$(jq -r '.id // 0' <<<"$line" 2>/dev/null)" || id=0
    method="$(jq -r '.method // ""' <<<"$line" 2>/dev/null)" || method=""
    params="$(jq -c '.params // {}' <<<"$line" 2>/dev/null)" || params="{}"
    case "$method" in
        hello)
            printf '{"id":%s,"ok":true,"result":{"protocol":"corpus.environment/1","capabilities":["lifecycle.setup"]}}\n' "$id"
            ;;
        setup)
            printf '{"id":%s,"event":"progress","phase":"dependency_fetch","message":"lock present","completed":1,"total":2}\n' "$id"
            printf '{"id":%s,"event":"progress","phase":"verification","message":"ready","completed":2,"total":2}\n' "$id"
            printf '{"id":%s,"ok":true,"result":{"ready":true}}\n' "$id"
            ;;
        doctor)
            printf '{"id":%s,"ok":false,"error":{"code":"docker_unavailable","message":"Docker is not running","retryable":true}}\n' "$id"
            ;;
        operation_status)
            key="$(jq -r '.idempotency_key // ""' <<<"$params")"
            jq -nc --argjson id "$id" --arg key "$key" \
                '{id:$id,ok:true,result:{idempotency_key:$key,state:"succeeded",result:{ready:true}}}'
            ;;
        status)
            printf '{"id":%s,"ok":true,"result":{"ready":true}}\n' "$((id + 1))"
            ;;
        stop)
            sleep 5
            printf '{"id":%s,"ok":true,"result":null}\n' "$id"
            ;;
        *)
            printf '{"id":%s,"ok":false,"error":{"code":"unknown_method","message":"unknown method","retryable":false}}\n' "$id"
            ;;
    esac
done
