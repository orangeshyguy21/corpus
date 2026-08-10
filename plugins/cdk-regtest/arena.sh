#!/usr/bin/env bash
# arena.sh — manages the corpus cdk-regtest "arena": the isolated container
# networks where agents execute attacks against the CDK regtest environment.
#
# Absorbed from cdk/vul-lab/bin/vul-lab (2026-08-10). All vul-lab-* names
# are renamed corpus-*; the orchestrator and run/mission machinery are not
# ported (opencode is the agent runner now). Configuration lives in
# config.toml, plugin-local evidence/ is mounted into the sandbox.
set -euo pipefail

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${CORPUS_CONFIG:-$PLUGIN_DIR/config.toml}"
AGENT_IMAGE="corpus-agent:local"
GW_NAME="corpus-target-gw"
EVIDENCE_DIR="$PLUGIN_DIR/evidence"
TOOLS_DIR="$PLUGIN_DIR/tools"
SOURCES_DIR="$(cd "$PLUGIN_DIR/../.." && pwd)/sources"
SOURCES_MANIFEST="${CORPUS_SOURCES_MANIFEST:-$(cd "$PLUGIN_DIR/../.." && pwd)/sources.toml}"

usage() {
    cat <<'EOF'
corpus cdk-regtest arena

  arena.sh doctor        Check prerequisites and probe sandbox isolation
  arena.sh up            Create arena networks, gateway, and agent image
  arena.sh down          Remove agent containers and arena networks
  arena.sh agent <job> [--egress|--no-egress] [--detach] [-- cmd...]
                        Start a locked-down agent container for a job
                        (testing|research); default cmd is a bash shell
  arena.sh status        Show arena state
EOF
}

die() { echo "corpus arena: error: $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Config access
#
# Minimal flat-TOML reader: `key = value` under `[section]` headers.
# Keep config.toml to this subset — no arrays, no nested tables.
# ---------------------------------------------------------------------------
toml_get() {
    local section="$1" key="$2"
    [ -f "$CONFIG" ] || die "config not found: $CONFIG"
    awk -v section="$section" -v key="$key" '
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

cfg() { # cfg <section> <key> <default>
    local val
    val="$(toml_get "$1" "$2")"
    printf '%s' "${val:-$3}"
}

need_docker() {
    command -v docker >/dev/null 2>&1 || die "docker not found (install OrbStack or Docker Desktop)"
    docker info >/dev/null 2>&1 || die "docker daemon not running"
}

url_port() {
    local hp="${1#*://}"
    hp="${hp%%/*}"
    local port="${hp##*:}"
    [ "$port" = "$hp" ] && port=80
    printf '%s' "$port"
}

# url_retarget <url> <host> — swap the host:port of a URL, keep scheme/path.
url_retarget() {
    local url="$1" host="$2" scheme rest hp port path
    scheme="${url%%://*}"
    rest="${url#*://}"
    hp="${rest%%/*}"
    port="${hp##*:}"; [ "$port" = "$hp" ] && port=80
    path="/${rest#*/}"; [ "$path" = "/$rest" ] && path=""
    printf '%s://%s:%s%s' "$scheme" "$host" "$port" "$path"
}

# Start the target gateway: the ONLY off-bridge path available to agents on
# the internal network. socat forwards exactly the configured target ports
# (the two mints) to the host. There is no general-purpose proxy, so
# internet egress stays denied — the agent can talk to the mints and
# nothing else. The model is deliberately NOT reachable from inside the
# arena: inference is orchestrated host-side.
start_gateway() {
    local net egress_net ports
    net="$(cfg arena network corpus-arena)"
    egress_net="$(cfg arena egress_network corpus-arena-egress)"
    ports="$(url_port "$(cfg target mint_url http://127.0.0.1:8085)")"
    ports="$ports $(url_port "$(cfg target mint_url_2 http://127.0.0.1:8087)")"
    # shellcheck disable=SC2086
    ports="$(echo $ports | tr ' ' '\n' | sort -u | tr '\n' ' ' | sed 's/ $//')"

    docker rm -f "$GW_NAME" >/dev/null 2>&1 || true
    # shellcheck disable=SC2086
    docker run -d --name "$GW_NAME" \
        --network "$net" \
        --user attacker --read-only \
        --cap-drop ALL --security-opt no-new-privileges \
        --add-host host.docker.internal:host-gateway \
        "$AGENT_IMAGE" bash -c \
        'for p in "$@"; do socat TCP-LISTEN:$p,fork,reuseaddr TCP:host.docker.internal:$p & done; wait' \
        _ $ports >/dev/null
    # Gateway straddles both networks: agents reach it over the internal
    # network, it reaches the host over the egress network.
    docker network connect "$egress_net" "$GW_NAME"
    echo "target gateway: $GW_NAME forwarding host ports $ports on $net"
}

# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

cmd_up() {
    need_docker
    local net egress_net
    net="$(cfg arena network corpus-arena)"
    egress_net="$(cfg arena egress_network corpus-arena-egress)"

    mkdir -p "$EVIDENCE_DIR" "$TOOLS_DIR"

    if docker network inspect "$net" >/dev/null 2>&1; then
        echo "network $net: exists"
    else
        # --internal: no default route off the bridge. This is the egress
        # kill switch for the testing job.
        docker network create --internal "$net" >/dev/null
        echo "network $net: created (internal, egress denied)"
    fi

    if docker network inspect "$egress_net" >/dev/null 2>&1; then
        echo "network $egress_net: exists"
    else
        docker network create "$egress_net" >/dev/null
        echo "network $egress_net: created (egress allowed)"
    fi

    echo "building $AGENT_IMAGE (cached if unchanged)..."
    docker build -t "$AGENT_IMAGE" \
        -f "$PLUGIN_DIR/arena/agent.Dockerfile" \
        "$PLUGIN_DIR/arena"

    start_gateway

    echo
    echo "arena up. Next: arena.sh doctor"
}

cmd_down() {
    need_docker
    local net egress_net
    net="$(cfg arena network corpus-arena)"
    egress_net="$(cfg arena egress_network corpus-arena-egress)"

    docker ps -aq --filter "name=^/corpus-" | xargs -r docker rm -f >/dev/null 2>&1 || true
    docker network rm "$net" >/dev/null 2>&1 && echo "removed $net" || true
    docker network rm "$egress_net" >/dev/null 2>&1 && echo "removed $egress_net" || true
    echo "arena down (image $AGENT_IMAGE kept; docker image rm to remove)"
}

cmd_agent() {
    need_docker
    local job="${1:-}"
    [ -n "$job" ] || die "usage: arena.sh agent <testing|research> [--egress|--no-egress] [-- cmd...]"
    shift

    local egress_override="" detach=false
    while [ $# -gt 0 ]; do
        case "$1" in
            --egress) egress_override="true"; shift ;;
            --no-egress) egress_override="false"; shift ;;
            --detach) detach=true; shift ;;
            --) shift; break ;;
            *) break ;;
        esac
    done

    local egress mem cpus pids
    egress="$(cfg "job.$job" egress false)"
    [ -n "$egress_override" ] && egress="$egress_override"
    mem="$(cfg "job.$job" memory 4g)"
    cpus="$(cfg "job.$job" cpus 2)"
    pids="$(cfg "job.$job" pids_limit 256)"

    local net
    if [ "$egress" = "true" ]; then
        net="$(cfg arena egress_network corpus-arena-egress)"
    else
        net="$(cfg arena network corpus-arena)"
    fi
    docker network inspect "$net" >/dev/null 2>&1 || die "network $net missing; run: arena.sh up"
    docker image inspect "$AGENT_IMAGE" >/dev/null 2>&1 || die "image missing; run: arena.sh up"
    docker inspect "$GW_NAME" >/dev/null 2>&1 || die "target gateway not running; run: arena.sh up"

    # Agents never see real host addresses — only the gateway. No model
    # endpoint is passed in: inference is an orchestrator-side concern.
    local mint_url mint_url_2
    mint_url="$(url_retarget "$(cfg target mint_url http://127.0.0.1:8085)" "$GW_NAME")"
    mint_url_2="$(url_retarget "$(cfg target mint_url_2 http://127.0.0.1:8087)" "$GW_NAME")"

    docker rm -f "corpus-sandbox-$job" >/dev/null 2>&1 || true

    local run_args=(
        --rm
        --name "corpus-sandbox-$job"
        --network "$net"
        --user attacker
        --read-only
        --tmpfs /tmp:rw,nosuid,nodev,size=512m
        --cap-drop ALL
        --security-opt no-new-privileges
        --memory "$mem"
        --cpus "$cpus"
        --pids-limit "$pids"
        --add-host host.docker.internal:host-gateway
        -v "$EVIDENCE_DIR:/evidence:rw"
        -v "$TOOLS_DIR:/opt/tools:ro"
        -e "CORPUS_JOB=$job"
        -e "CORPUS_EGRESS=$egress"
        -e "CDK_TARGET_MINT_URL=$mint_url"
        -e "CDK_TARGET_MINT_URL_2=$mint_url_2"
        -e "EVIDENCE_DIR=/evidence"
    )
    # Pinned source corpus: /opt/src/<name>, read-only.
    while IFS= read -r mount; do
        run_args+=("$mount")
    done < <(source_mount_args)
    if [ "$detach" = true ]; then
        run_args+=(-d)
    elif [ -t 1 ]; then
        run_args+=(-it)
    else
        run_args+=(-i)
    fi

    echo "job=$job egress=$egress net=$net mem=$mem cpus=$cpus pids=$pids" >&2
    docker run "${run_args[@]}" "$AGENT_IMAGE" "$@"
    if [ "$detach" = true ]; then
        echo "detached agent container: corpus-sandbox-$job" >&2
    fi
}

# source_mount_args — the pinned research corpus at /opt/src/<name>, read-only.
# Each host tree is sources/<name>/<sha>; config.toml [sources] selects the
# sha, sources.toml is the manifest. Die loudly on missing pins: an agent
# must never silently run without the source the mission pins assume.
source_mount_args() {
    local args=() name sha
    for name in cdk nuts; do
        sha="$(cfg sources "${name}_sha" '')"
        [ -n "$sha" ] || continue
        local tree="$SOURCES_DIR/$name/$sha"
        if [ ! -d "$tree/.git" ]; then
            die "sources/$name/$sha not fetched — run: bash plugins/cdk-regtest/setup.sh"
        fi
        args+=(-v "$tree:/opt/src/$name:ro")
    done
    printf '%s\n' "${args[@]}"
}

# Probe: run curl from an ephemeral agent container on a given network.
# Prints "ok" if the URL is reachable, "fail" otherwise.
probe() {
    local network="$1" url="$2"
    if docker run --rm --network "$network" "$AGENT_IMAGE" \
        curl -sf --max-time 4 -o /dev/null "$url" >/dev/null 2>&1; then
        printf 'ok'
    else
        printf 'fail'
    fi
}

cmd_doctor() {
    local failures=0
    check() { # check <ok|warn|fail> <message>
        case "$1" in
            ok) echo "  [ok]   $2" ;;
            warn) echo "  [warn] $2" ;;
            fail) echo "  [FAIL] $2"; failures=$((failures + 1)) ;;
        esac
    }

    echo "prerequisites"
    if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
        check ok "docker daemon ($(docker context show 2>/dev/null || echo default))"
    else
        check fail "docker daemon not available"
    fi
    for tool in bash curl jq awk; do
        command -v "$tool" >/dev/null 2>&1 && check ok "$tool" || check fail "$tool missing (host)"
    done
    [ -f "$CONFIG" ] && check ok "config: $CONFIG" || check fail "config missing: $CONFIG"

    echo "config"
    echo "  testing:  egress=$(cfg job.testing egress '?') mem=$(cfg job.testing memory '?') cpus=$(cfg job.testing cpus '?')"
    echo "  research: egress=$(cfg job.research egress '?') mem=$(cfg job.research memory '?') cpus=$(cfg job.research cpus '?')"

    echo "targets (host)"
    local mint_url mint_url_2
    mint_url="$(cfg target mint_url '')"
    mint_url_2="$(cfg target mint_url_2 '')"
    if curl -sf --max-time 3 "$mint_url/v1/info" >/dev/null 2>&1; then
        check ok "mint 1 reachable: $mint_url"
    else
        check warn "mint 1 unreachable: $mint_url (start with: just regtest)"
    fi
    if curl -sf --max-time 3 "$mint_url_2/v1/info" >/dev/null 2>&1; then
        check ok "mint 2 reachable: $mint_url_2"
    else
        check warn "mint 2 unreachable: $mint_url_2"
    fi

    echo "sources (pinned research corpus)"
    local name sha tree
    for name in cdk nuts; do
        sha="$(cfg sources "${name}_sha" '')"
        if [ -z "$sha" ]; then
            check warn "sources/$name: not configured in config.toml [sources]"
            continue
        fi
        tree="$SOURCES_DIR/$name/$sha"
        if [ ! -d "$tree/.git" ]; then
            check warn "sources/$name/$sha not fetched (run: plugins/cdk-regtest/setup.sh)"
            continue
        fi
        local head
        head="$(git -C "$tree" rev-parse HEAD 2>/dev/null || true)"
        if [ "$head" = "$sha" ]; then
            check ok "sources/$name: HEAD == pin ($sha)"
        else
            check fail "sources/$name: HEAD $head != pinned $sha — re-fetch"
        fi
    done

    echo "arena"
    local net egress_net
    net="$(cfg arena network corpus-arena)"
    egress_net="$(cfg arena egress_network corpus-arena-egress)"
    local nets_up=true image_up=true
    docker network inspect "$net" >/dev/null 2>&1 && check ok "network $net" || { check warn "network $net missing (run: arena.sh up)"; nets_up=false; }
    docker network inspect "$egress_net" >/dev/null 2>&1 && check ok "network $egress_net" || { check warn "network $egress_net missing"; nets_up=false; }
    docker image inspect "$AGENT_IMAGE" >/dev/null 2>&1 && check ok "image $AGENT_IMAGE" || { check warn "image missing (run: arena.sh up)"; image_up=false; }

    # Sandbox self-verification: prove the isolation claims, don't assume.
    if [ "$nets_up" = true ] && [ "$image_up" = true ]; then
        echo "isolation probes (ephemeral containers)"
        if [ "$(probe "$net" https://example.com)" = fail ]; then
            check ok "internal network denies internet egress"
        else
            check fail "internal network ALLOWS internet egress — testing job is not isolated"
        fi
        if [ "$(probe "$egress_net" https://example.com)" = ok ]; then
            check ok "egress network reaches internet (research job)"
        else
            check warn "egress network cannot reach internet"
        fi
        if docker inspect "$GW_NAME" >/dev/null 2>&1; then
            check ok "target gateway running"
            if curl -sf --max-time 3 "$mint_url/v1/info" >/dev/null 2>&1; then
                local gw_url
                gw_url="$(url_retarget "$mint_url" "$GW_NAME")"
                if [ "$(probe "$net" "$gw_url/v1/info")" = ok ]; then
                    check ok "mint reachable from internal network via gateway"
                else
                    check fail "mint NOT reachable from internal network ($gw_url)"
                fi
            fi
        else
            check warn "target gateway not running (run: arena.sh up)"
        fi
    fi

    echo
    if [ "$failures" -gt 0 ]; then
        echo "doctor: $failures failure(s)"; exit 1
    fi
    echo "doctor: healthy"
}

cmd_status() {
    need_docker
    echo "networks:"
    docker network ls --filter "name=corpus-arena" --format '  {{.Name}} ({{.Driver}} internal={{.Internal}})'
    echo "containers:"
    docker ps -a --filter "name=corpus-" --format '  {{.Names}} {{.Status}}'
    echo "evidence: $EVIDENCE_DIR ($(du -sh "$EVIDENCE_DIR" 2>/dev/null | cut -f1 || echo empty))"
    echo "sources: $SOURCES_DIR"
    local name sha head
    for name in cdk nuts; do
        sha="$(cfg sources "${name}_sha" '')"
        if [ -n "$sha" ] && [ -d "$SOURCES_DIR/$name/$sha/.git" ]; then
            head="$(git -C "$SOURCES_DIR/$name/$sha" rev-parse HEAD 2>/dev/null || echo ?)"
            echo "  $name: mounted $sha (HEAD $head)"
        else
            echo "  $name: not fetched"
        fi
    done
}

main() {
    local cmd="${1:-help}"
    shift || true
    case "$cmd" in
        up) cmd_up "$@" ;;
        down) cmd_down "$@" ;;
        agent) cmd_agent "$@" ;;
        doctor) cmd_doctor "$@" ;;
        status) cmd_status "$@" ;;
        help | --help | -h) usage ;;
        *) die "unknown command: $cmd (try: arena.sh help)" ;;
    esac
}

main "$@"