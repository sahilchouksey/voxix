#!/usr/bin/env bash
# health.sh — liveness + bearer-authed health/status probes.
#
# Demonstrates:
#   GET /alive       — unauthenticated liveness (supervisors/proxies)
#   GET /v1/health   — bearer-authed: version, engine_version, protocol_version, uptime
#   GET /v1/status   — bearer-authed: mode/bind/active model, lease, capabilities
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   ./examples/api/health.sh
#
# Expected output: three pretty-printed JSON documents.
#   { "alive": true }
#   { "status": "ok", "version": "…", "engine_version": "…+<sha>",
#     "protocol_version": 1, "uptime_ms": N, "ready": true }
#   { "mode": "…", "bind": "127.0.0.1:…", "active_model_id": …, "model_state": "…",
#     "capabilities": { "max_concurrent_streams": 1, … }, … }
# Exit codes: 0 ok · 1 curl/auth failure (401 = missing/wrong token) · 2 usage
# Note: /alive never needs a token; every other /v1/* route does (401 + empty body on failure).
set -euo pipefail

BASE="${VOXIX_BASE_URL:-http://127.0.0.1:47832}"
TOKEN="${VOXIX_TOKEN:-}"
if [ -z "$TOKEN" ]; then
  echo "error: VOXIX_TOKEN is not set (expected the vx_… bearer printed at first daemon start)" >&2
  exit 2
fi
pretty() { python3 -m json.tool; }   # jq-free JSON pretty-printer

echo '--- GET /alive (no auth) ---'
curl -sS -f "$BASE/alive" | pretty

echo '--- GET /v1/health (bearer) ---'
curl -sS -f -H "Authorization: Bearer $TOKEN" "$BASE/v1/health" | pretty

echo '--- GET /v1/status (bearer) ---'
curl -sS -f -H "Authorization: Bearer $TOKEN" "$BASE/v1/status" | pretty
