#!/usr/bin/env bash
# sessions.sh — the session lifecycle over raw HTTP (create → get → complete).
#
# Demonstrates:
#   POST   /v1/sessions        — admission-controlled create. Body is
#                                {"model_id"|"model"?, "language"?} plus the M2 keys
#                                (task/audio/vad/partial_results/save_history/
#                                client_session_key); unknown fields → 422.
#                                503 service_full (admission) / 503 service_busy
#                                (single-inference lease) / 503 engine_error otherwise.
#                                → 201 {session_id, session_token, state:"created",
#                                        ws_url, negotiated_audio, streaming_mode, …}
#   GET    /v1/sessions/{id}   — projection; needs bearer + X-Session-Token.
#                                Wrong/missing sub-token or unknown id → 404
#                                session_not_found (never 403 — ids cannot be probed)
#   PATCH  /v1/sessions/{id}   — {"state":"completed"} → 200 projection (lease released)
#                                {"state":"cancelled"} → 204 (lease released)
#                                unknown state → 422
#   DELETE /v1/sessions/{id}   — idempotent cancel → 204; terminal record retained
#                                for the retention window, then drops out of the registry
#
# The WS media plane (audio in, partials/finals out) is NOT usable with curl —
# it needs a WebSocket client that speaks the protocol-v1 frame discipline
# . See ../sdk/05-streaming.ts for the working path.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/sessions.sh [completed|cancelled|delete]   (default: completed)
#
# Expected output: the 201 envelope (contains a one-time session_token), then the
# GET/PATCH/DELETE exchange for the chosen action.
# Exit codes: 0 ok · 1 curl/auth failure · 2 usage
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
ACTION="${1:-completed}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set" >&2
  exit 2
fi
case "$ACTION" in completed|cancelled|delete) ;; *) echo "usage: $0 [completed|cancelled|delete]" >&2; exit 2;; esac

echo '--- POST /v1/sessions ---'
CREATED="$(curl -sS -f -X POST \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  --data '{}' \
  "$BASE/v1/sessions")"
printf '%s\n' "$CREATED" | python3 -m json.tool
SESSION_ID="$(printf '%s\n' "$CREATED" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session_id"])')"
SESSION_TOKEN="$(printf '%s\n' "$CREATED" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session_token"])')"

echo '--- GET /v1/sessions/{id} (bearer + X-Session-Token) ---'
curl -sS -f \
  -H "Authorization: Bearer $TOKEN" \
  -H "X-Session-Token: $SESSION_TOKEN" \
  "$BASE/v1/sessions/$SESSION_ID" | python3 -m json.tool

case "$ACTION" in
  completed)
    echo '--- PATCH /v1/sessions/{id} {"state":"completed"} ---'
    curl -sS -f -X PATCH \
      -H "Authorization: Bearer $TOKEN" \
      -H "X-Session-Token: $SESSION_TOKEN" \
      -H 'Content-Type: application/json' \
      --data '{"state":"completed"}' \
      "$BASE/v1/sessions/$SESSION_ID" | python3 -m json.tool
    ;;
  cancelled)
    echo '--- PATCH /v1/sessions/{id} {"state":"cancelled"} (expect 204, empty body) ---'
    CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X PATCH \
      -H "Authorization: Bearer $TOKEN" \
      -H "X-Session-Token: $SESSION_TOKEN" \
      -H 'Content-Type: application/json' \
      --data '{"state":"cancelled"}' \
      "$BASE/v1/sessions/$SESSION_ID")"
    echo "HTTP $CODE"
    ;;
  delete)
    echo '--- DELETE /v1/sessions/{id} (idempotent; expect 204) ---'
    CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE \
      -H "Authorization: Bearer $TOKEN" \
      -H "X-Session-Token: $SESSION_TOKEN" \
      "$BASE/v1/sessions/$SESSION_ID")"
    echo "HTTP $CODE"
    echo '--- DELETE again (still 204 — idempotent) ---'
    CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE \
      -H "Authorization: Bearer $TOKEN" \
      -H "X-Session-Token: $SESSION_TOKEN" \
      "$BASE/v1/sessions/$SESSION_ID")"
    echo "HTTP $CODE"
    ;;
esac
