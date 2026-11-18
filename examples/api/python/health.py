#!/usr/bin/env python3
# health.py — liveness + bearer-authed health/status probes.
#
# Demonstrates:
#   GET /alive       — unauthenticated liveness (supervisors/proxies)
#   GET /v1/health   — bearer-authed: version, engine_version, protocol_version, uptime, ready
#   GET /v1/status   — bearer-authed: mode/bind/active model, model_state, lease, capabilities
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   python3 examples/api/python/health.py
#
# Expected output: three pretty-printed JSON documents.
#   { "alive": true }
#   { "status": "ok", "version": "…", "engine_version": "…+<sha>",
#     "protocol_version": 1, "uptime_ms": N, "ready": true }
#   { "mode": "…", "bind": "127.0.0.1:…", "active_model_id": …, "model_state": "…",
#     "capabilities": { "max_concurrent_streams": 1, … }, … }
# Exit codes: 0 ok · 1 curl-equivalent failure (401 = missing/wrong token) · 2 usage
# Note: /alive never needs a token; every other /v1/* route does (401 + empty body on failure).
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("VOXIX_BASE_URL", "http://127.0.0.1:47832").rstrip("/")
TOKEN = os.environ.get("VOXIX_TOKEN")
if not TOKEN:
    sys.exit("error: VOXIX_TOKEN is not set (expected the vx_… bearer printed at first daemon start)")


def get(path, auth):
    req = urllib.request.Request(BASE + path, method="GET")
    if auth:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        # Non-2xx /v1 bodies carry {"error":{"code","message","retryable","request_id","details?"}}.
        # 401 is empty by design (no probe enrichment).
        return e.code, e.read().decode("utf-8", "replace")
    except urllib.error.URLError as e:
        sys.exit(f"error: cannot reach {BASE} ({e.reason})")


def show(label, status, body):
    print(f"--- {label} → HTTP {status} ---")
    try:
        print(json.dumps(json.loads(body), indent=2))
    except json.JSONDecodeError:
        print(body if body else "(empty body)")
    if status >= 400:
        sys.exit(1)


status, body = get("/alive", auth=False)
show("GET /alive (no auth)", status, body)

status, body = get("/v1/health", auth=True)
show("GET /v1/health (bearer)", status, body)

status, body = get("/v1/status", auth=True)
show("GET /v1/status (bearer)", status, body)
