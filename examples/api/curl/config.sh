#!/usr/bin/env bash
# config.sh — inspect and (non-destructively) patch the daemon config.
#
# Demonstrates:
#   GET   /v1/config   — the read projection (no secrets)
#   PATCH /v1/config   — the mutable set on this build:
#                        {allowed_origins, max_sessions (1–64), max_audio_seconds (1–7200),
#                         session_resume_window_ms (0–120000), pause_max_ms (0 or ≤3600000),
#                         save_history_default, vad_enabled_default, opus_enabled}
#                        Unknown fields answer 422; `bind` is not patchable (422).
#                        `opus_enabled:true` without the service-opus build answers 422.
#
# This script performs a NO-OP PATCH: it reads the current `max_audio_seconds`
# and writes the same value back, so daemon behavior is unchanged. Replace the
# value (keep the key inside the mutable set) to change it for real.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/config.sh
#
# Expected output: the config JSON before and after the patch (identical).
# Exit codes: 0 ok · 1 curl/auth/422 failure · 2 usage
# Note: protocol-v1.md §4 lists GET /v1/config as a "read-only projection (M1)";
# the PATCH route landed with M3 and is live on this build — docs updated here.
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set" >&2
  exit 2
fi
pretty() { python3 -m json.tool; }

echo '--- GET /v1/config ---'
BEFORE="$(curl -sS -f -H "Authorization: Bearer $TOKEN" "$BASE/v1/config")"
printf '%s\n' "$BEFORE" | pretty

# Extract current max_audio_seconds (422 if the shape ever changes — fail loudly).
CURRENT="$(printf '%s\n' "$BEFORE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["max_audio_seconds"])')"
echo "--- PATCH /v1/config {\"max_audio_seconds\": $CURRENT}  (same-value no-op) ---"

# Note: PATCH is NOT idempotent for read-only builds of the daemon; if your
# build still rejects PATCH it answers 422 invalid_request and this step fails —
# that is the signal the daemon predates M3 config.
curl -sS -f -X PATCH \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  --data "{\"max_audio_seconds\": $CURRENT}" \
  "$BASE/v1/config" | pretty
