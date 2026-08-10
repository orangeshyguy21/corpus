#!/usr/bin/env bash
# Oracle 010: no secret material in captured logs.
#
# Scans log captures in the evidence directory for high-signal secret
# patterns. Proof secrets and signatures are legitimately present in
# protocol traffic, so this intentionally matches only unambiguous leak
# markers (key files, explicit secret fields) to avoid false positives.
set -uo pipefail

LOG_ROOT="${EVIDENCE_DIR:?}/logs"
if [ ! -d "$LOG_ROOT" ] || [ -z "$(ls -A "$LOG_ROOT" 2>/dev/null)" ]; then
    echo "no captured logs in $LOG_ROOT"
    exit 2
fi

# Deliberately narrow patterns: private key blocks, explicit secret fields,
# seed phrases. 64-char hex alone is NOT matched (protocol payloads are hex).
PATTERNS='BEGIN [A-Z ]*PRIVATE KEY|secret_key"?\s*[:=]|private_key"?\s*[:=]|xprv[A-Za-z0-9]{20,}|mnemonic"?\s*[:=]'

if grep -rEn "$PATTERNS" "$LOG_ROOT" >/dev/null 2>&1; then
    echo "secret material pattern found in captured logs:"
    grep -rEn "$PATTERNS" "$LOG_ROOT" | head -20
    exit 1
fi
