#!/usr/bin/env bash
set -uo pipefail
while IFS= read -r line; do
  id="$(jq -r '.id // 0' <<<"$line")"
  method="$(jq -r '.method // ""' <<<"$line")"
  case "$method" in
    probe) printf '{"id":%s,"ok":true,"result":{"ready":true,"notes":"noop ready"}}\n' "$id" ;;
    sources) printf '{"id":%s,"ok":true,"result":[]}\n' "$id" ;;
    targets|tools|oracles) printf '{"id":%s,"ok":true,"result":[]}\n' "$id" ;;
    *) printf '{"id":%s,"ok":false,"error":"unsupported fixture method"}\n' "$id" ;;
  esac
done

