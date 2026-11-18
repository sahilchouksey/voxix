#!/usr/bin/env bash
# models.sh — the model catalog surface.
#
# Demonstrates:
#   GET  /v1/models?installed=true   — list installed models only
#                                      (wire shape is `installed=true|false`, NOT filter[…];
#                                       omit the query param to list everything)
#   GET  /v1/models/{id}             — one model + prepare-stage projection
#                                      (ids may contain slashes, e.g. ggml/whisper — the
#                                       wildcard route decodes percent-escapes once)
#   PUT  /v1/models/selected         — select + eagerly load a model
#                                      body: {"model_id": "…", "load": "eager"|"lazy"}
#                                      ("eager" default; PUT — not POST)
#                                      409 model_not_ready when not installed locally —
#                                      the service never implicitly downloads
#   GET  /v1/models/{id}/downloads   — managed-download status projection
#                                      {state,downloaded_bytes,total_bytes,percentage?}
#                                      (POST/DELETE /v1/models/{id}/download start/cancel
#                                       a managed download — not exercised here on purpose;
#                                       see sdk/03-models.ts)
#
# Model ids may contain `/`; quote them and let curl pass them raw (the server
# decodes exactly once). MODEL_ID below defaults to the first installed model.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/models.sh [MODEL_ID]
#
# Expected output: models list, one-model projection, selection echo, download status.
# Exit codes: 0 ok · 1 curl/auth failure · 2 usage
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set" >&2
  exit 2
fi
pretty() { python3 -m json.tool; }
AUTH=(-H "Authorization: Bearer $TOKEN")

echo '--- GET /v1/models?installed=true ---'
MODELS_JSON="$(curl -sS -f "${AUTH[@]}" "$BASE/v1/models?installed=true")"
printf '%s\n' "$MODELS_JSON" | pretty

MODEL_ID="${1:-$(printf '%s\n' "$MODELS_JSON" | python3 -c 'import json,sys; m=json.load(sys.stdin)["models"]; print(m[0]["id"] if m else "")')}"
if [ -z "$MODEL_ID" ]; then
  echo "error: no installed models found and no MODEL_ID argument given" >&2
  echo "       (the daemon never implicitly downloads; install a model in the app first)" >&2
  exit 2
fi
echo "(using MODEL_ID=$MODEL_ID)"

echo "--- GET /v1/models/$MODEL_ID ---"
curl -sS -f "${AUTH[@]}" "$BASE/v1/models/$MODEL_ID" | pretty

echo "--- GET /v1/models/$MODEL_ID/downloads ---"
curl -sS -f "${AUTH[@]}" "$BASE/v1/models/$MODEL_ID/downloads" | pretty

# PUT /v1/models/selected MUTATES daemon state (it eagerly loads the model on this
# build, and 409s during an inference lease). Opt in with SELECT=1:
if [ "${SELECT:-0}" = "1" ]; then
  echo "--- PUT /v1/models/selected {\"model_id\": \"$MODEL_ID\", \"load\": \"eager\"} ---"
  curl -sS -f -X PUT \
    "${AUTH[@]}" \
    -H 'Content-Type: application/json' \
    --data "{\"model_id\": \"$MODEL_ID\", \"load\": \"eager\"}" \
    "$BASE/v1/models/selected" | pretty
else
  echo "(skipping PUT /v1/models/selected — set SELECT=1 to run it; it loads the model)"
fi
