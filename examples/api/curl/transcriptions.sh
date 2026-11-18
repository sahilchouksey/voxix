#!/usr/bin/env bash
# transcriptions.sh — batch (non-streaming) transcription over raw HTTP.
#
# Demonstrates:
#   POST   /v1/transcriptions          — upload a raw WAV body (Content-Type
#                                        application/octet-stream); 64 MiB cap;
#                                        202 {job_id, state:"processing", status_url}
#                                        Rejections: 429 queue-full (another batch/prepare
#                                        in flight) · 409 service_busy (live session holds
#                                        the inference lease) · 400 invalid-audio (bad WAV)
#   GET    /v1/transcriptions/{job}    — poll; states: processing → completed|failed|cancelled
#                                        404 not_found for unknown ids
#   DELETE /v1/transcriptions/{job}    — best-effort cancel: 204 while processing,
#                                        409 job_settled once terminal, 404 unknown
#
# Wire fidelity notes:
#   * The body is the WAV file bytes verbatim — no multipart, no JSON.
#   * Error bodies always carry {"error":{code,message,retryable,request_id,details?}}.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/transcriptions.sh ./speech.wav
#   DELETE_AFTER_START=1 ./examples/api/transcriptions.sh ./speech.wav   # exercises the cancel path
#
# Expected output: 202 envelope, poll lines, then the transcript (or a clear failure).
# Exit codes: 0 completed · 1 failed/cancelled/timed out · 2 usage
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
WAV="${1:-}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set" >&2
  exit 2
fi
if [ -z "$WAV" ] || [ ! -f "$WAV" ]; then
  echo "usage: $0 <file.wav>" >&2
  exit 2
fi
AUTH=(-H "Authorization: Bearer $TOKEN")

echo "--- POST /v1/transcriptions ($(wc -c <"$WAV" | tr -d ' ') bytes) ---"
ACCEPTED="$(curl -sS -f "${AUTH[@]}" \
  -H 'Content-Type: application/octet-stream' \
  --data-binary @"$WAV" \
  "$BASE/v1/transcriptions")"
printf '%s\n' "$ACCEPTED" | python3 -m json.tool
JOB_ID="$(printf '%s\n' "$ACCEPTED" | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])')"

if [ "${DELETE_AFTER_START:-0}" = "1" ]; then
  # Demonstrates the cancel path (best-effort: 204 if still processing, else 409).
  echo "--- DELETE /v1/transcriptions/$JOB_ID ---"
  CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "${AUTH[@]}" "$BASE/v1/transcriptions/$JOB_ID")"
  echo "HTTP $CODE"
  exit 0
fi

echo "--- GET /v1/transcriptions/$JOB_ID (polling) ---"
DEADLINE=$((SECONDS + 300))
while :; do
  RESP="$(curl -sS -f "${AUTH[@]}" "$BASE/v1/transcriptions/$JOB_ID")"
  STATE="$(printf '%s\n' "$RESP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])')"
  case "$STATE" in
    completed)
      printf '%s\n' "$RESP" | python3 -m json.tool
      echo "--- transcript ---"
      printf '%s\n' "$RESP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["transcript"])'
      exit 0
      ;;
    failed|cancelled)
      printf '%s\n' "$RESP" | python3 -m json.tool >&2
      echo "job ended in state: $STATE" >&2
      exit 1
      ;;
    processing)
      if [ "$SECONDS" -ge "$DEADLINE" ]; then
        echo "timed out after 300 s (job is still processing; retry or DELETE $JOB_ID)" >&2
        exit 1
      fi
      printf '  … %s\n' "$STATE"
      sleep 0.25
      ;;
    *)
      echo "unexpected state: $STATE" >&2
      exit 1
      ;;
  esac
done
