#!/usr/bin/env bash
# ws-ticket.sh — mint a one-use ticket for the WebSocket upgrade.
#
# Demonstrates:
#   POST /v1/auth/ws-ticket  {"session_id"?: string} → 201
#     {ticket, expires_at} — a 60-second, one-use credential for upgrading
#     GET /v1/sessions/{id}/stream with `?ticket=…` instead of the bearer
#     header (runtimes where Authorization cannot be attached to a socket).
#
# Semantics ( §11 + bearer middleware):
#   * Bearer remains required for REST; the ticket substitutes for the bearer
#     ONLY on the WS upgrade path — and only when it is valid, unexpired and
#     unconsumed. Consumption happens inside the upgrade handler.
#   * With session_id set, the ticket is bound to that session; consuming it
#     against any other session id fails.
#   * X-Session-Token is STILL required on the upgrade — the ticket only
#     replaces the bearer.
#   * A second socket per session is refused 503 service_full.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/ws-ticket.sh                 # unbound ticket
#   ./examples/api/ws-ticket.sh <session-id>    # ticket bound to that session
#
# Expected output: the 201 body. NOTE the ticket is single-use — do not retry it.
# Exit codes: 0 ok · 1 curl/auth failure · 2 usage
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
SESSION_ID="${1:-}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set" >&2
  exit 2
fi

if [ -n "$SESSION_ID" ]; then
  BODY="{\"session_id\": \"$SESSION_ID\"}"
else
  BODY='{}'
fi

echo '--- POST /v1/auth/ws-ticket (expect 201) ---'
curl -sS -f -X POST \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  --data "$BODY" \
  "$BASE/v1/auth/ws-ticket" | python3 -m json.tool
