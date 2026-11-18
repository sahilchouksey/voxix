#!/usr/bin/env bash
# history.sh — paginated transcript history.
#
# Demonstrates:
#   GET /v1/history?limit=N&cursor=M — entries newest-first; page shape
#                                      {entries:[…], has_more, next_cursor?}.
#                                      `limit` clamps to 1–100 (default 20);
#                                      pass `next_cursor` back as `cursor` to
#                                      walk older pages.
#   GET /v1/history/{id}             — one entry by integer id (404 not_found otherwise)
#
# Entries are written by the daemon when a batch job completes or a stream
# finalizes with save_history on (see save_history_default in /v1/config).
# The route may answer 503 history_unavailable on builds without the history
# database — that is a capability signal, not a token problem.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/history.sh [LIMIT] [ENTRY_ID]
#
# Expected output: the page JSON (or a limit-trimmed view), then one entry.
# Exit codes: 0 ok · 1 curl failure (incl. 404/503) · 2 usage
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
LIMIT="${1:-5}"
ENTRY_ID="${2:-}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set" >&2
  exit 2
fi
case "$LIMIT" in (*[!0-9]*|'') echo "usage: $0 [LIMIT] [ENTRY_ID]  (LIMIT must be an integer)" >&2; exit 2;; esac

echo "--- GET /v1/history?limit=$LIMIT ---"
PAGE="$(curl -sS -f -H "Authorization: Bearer $TOKEN" "$BASE/v1/history?limit=$LIMIT")"
printf '%s\n' "$PAGE" | python3 -m json.tool
echo "(next page would be: GET /v1/history?limit=$LIMIT&cursor=<next_cursor>)"

# Pick the entry to fetch: the explicit argument, else the newest entry in the page.
FIRST_ID="$(printf '%s\n' "$PAGE" | python3 -c 'import json,sys; e=json.load(sys.stdin)["entries"]; print(e[0]["id"] if e else "")')"
TARGET="${ENTRY_ID:-$FIRST_ID}"
if [ -z "$TARGET" ]; then
  echo "(no entries yet — history is empty; nothing to fetch individually)"
  exit 0
fi
echo "--- GET /v1/history/$TARGET ---"
curl -sS -f -H "Authorization: Bearer $TOKEN" "$BASE/v1/history/$TARGET" | python3 -m json.tool
