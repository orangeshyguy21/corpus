#!/usr/bin/env bash
# Builds Linux attack tools (cdk-cli) for the arena agent container.
#
# Runs the build inside a rust container: no host toolchain pollution, no
# cross-compile setup, and the output lands in tools/ which is mounted
# read-only at /opt/tools in agent containers. Named docker volumes cache
# the cargo registry and target dir so rebuilds are incremental.
#
# Absorbed from cdk/tools/build-tools.sh (2026-08-10). The source
# lives in the cdk repo (the target environment owner) via CORPUS_CDK_REPO.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${CORPUS_CDK_REPO:-$HOME/Sites/cdk}"
IMAGE="${CORPUS_TOOLCHAIN_IMAGE:-rust:1}"

[ -d "$REPO_ROOT" ] || { echo "build-tools: cdk repo not found at $REPO_ROOT (set CORPUS_CDK_REPO)" >&2; exit 1; }

# rust:1 and arena/agent.Dockerfile are both debian-stable based, so the
# dynamically linked binary matches the agent container's glibc.
docker run --rm \
    -v "$REPO_ROOT:/src:ro" \
    -v corpus-cargo-home:/cargo \
    -v corpus-cargo-target:/target \
    -v "$HERE:/out" \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -w /src \
    "$IMAGE" \
    bash -c 'set -e; cargo build --locked -p cdk-cli \
        && (strip /target/debug/cdk-cli 2>/dev/null || true) \
        && cp /target/debug/cdk-cli /out/cdk-cli'

echo "built: $HERE/cdk-cli"