#!/usr/bin/env bash
# Oracle 030: authentication is enforced on state-changing endpoints.
#
# Invariant: when NUT-21/NUT-22 auth is enabled, every state-changing
# endpoint (mint, melt, swap) rejects unauthenticated and malformed-auth
# requests. A violation means unauthorized value movement.
#
# TODO: implement. Requires an auth-enabled mint fixture — the regtest
# mints run without auth by default. The Keycloak setup in misc/keycloak/
# is the intended building block (see also: just fake-auth-mint-itest).
echo "not yet implemented"
exit 2
