#!/usr/bin/env bash
# tokens.sh — the token-mint/rotate surface (READ THIS BEFORE RUNNING).
#
# Demonstrates the shape of:
#   POST /v1/auth/tokens  {"name"?: string, "rotate_primary"?: bool} → 201
#     {id, name, token, prefix, created_at}  — `token` is the ONLY time the
#     plaintext appears; the daemon stores only its SHA-256 verifier.
#
# There is NO GET on this route. On this build:
#   * rotate_primary absent/false — mints a NAMED SUPPLEMENTAL token that is
#     returned to you but is NOT registered in the verifier chain: it will
#     NOT authenticate afterwards. Treat as experimental; do not use it.
#   * rotate_primary: true        — replaces the daemon's primary verifier:
#     every existing client (including this session's VOXIX_TOKEN and any
#     paired SDKs) is immediately cut over to the new token; extant WS
#     connections receive close code 4001. There is NO UNDO from this script.
#
# For those reasons this script is DRY-RUN by default and prints what it
# *would* send. To actually rotate (cutting over all clients):
#   ROTATE_PRIMARY=1 I_UNDERSTAND_THIS_INVALIDATES_ALL_TOKENS=1 ./examples/api/tokens.sh primary
#
# Ws-ticket for a single WS upgrade: see ws-ticket.sh — that is the safe,
# common-case companion to this file.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/tokens.sh [name]
#
# Exit codes: 0 dry-run/success · 1 curl/auth failure · 2 usage
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
NAME="${1:-supplemental}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set" >&2
  exit 2
fi

if [ "${ROTATE_PRIMARY:-0}" = "1" ]; then
  if [ "${I_UNDERSTAND_THIS_INVALIDATES_ALL_TOKENS:-0}" != "1" ]; then
    echo "refusing: set I_UNDERSTAND_THIS_INVALIDATES_ALL_TOKENS=1 to confirm cutover" >&2
    exit 2
  fi
  echo '--- POST /v1/auth/tokens {"name":…,"rotate_primary":true} (DESTRUCTIVE cutover) ---'
  echo '--- the 201 body below carries the NEW PRIMARY TOKEN; the old one is dead NOW.'
  echo '--- store it immediately; never commit it; there is no second chance.          ---'
  curl -sS -f -X POST \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    --data "{\"name\": \"$NAME\", \"rotate_primary\": true}" \
    "$BASE/v1/auth/tokens" | python3 -m json.tool
  exit 0
fi

echo "DRY-RUN (nothing sent). Would POST $BASE/v1/auth/tokens"
echo "  body: {\"name\": \"$NAME\"}"
echo "  effect (this build): mints an unverifiable supplemental token — see header notes"
echo
echo "Would rotate with: ROTATE_PRIMARY=1 I_UNDERSTAND_THIS_INVALIDATES_ALL_TOKENS=1 $0 $NAME"
echo "rotating invalidates every paired token and drops live WS connections (close 4001)"
