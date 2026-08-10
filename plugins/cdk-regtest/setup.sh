#!/usr/bin/env bash
# setup.sh — one-shot first-party setup for the cdk-regtest plugin.
#
#   1. Build the agent image + arena networks/gateway (arena.sh up)
#   2. Populate tools/ with the compiled attack tools (build-tools.sh)
#   3. Run the doctor self-verification probes
#
# The regtest environment itself (`just regtest` in the cdk repo) is the
# target environment the plugin *depends* on; it is not owned here. Point
# CORPUS_CDK_REPO at the cdk checkout if it is not ~/Sites/cdk.
set -euo pipefail

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "==> arena: building image, networks, gateway"
bash "$PLUGIN_DIR/arena.sh" up

if [ ! -x "$PLUGIN_DIR/tools/cdk-cli" ]; then
    echo "==> tools: building cdk-cli (first time; cached afterwards)"
    bash "$PLUGIN_DIR/tools/build-tools.sh"
else
    echo "==> tools: cdk-cli already present"
fi

echo "==> doctor: self-verification probes"
bash "$PLUGIN_DIR/arena.sh" doctor

echo "==> cdk-regtest plugin: setup complete"