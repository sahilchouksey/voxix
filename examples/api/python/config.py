#!/usr/bin/env python3
# config.py — inspect and (non-destructively) patch the daemon config.
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
#   python3 examples/api/python/config.py
#
# Expected output: the config JSON before and after the patch (identical).
# Exit codes: 0 ok · 1 request failure (incl. 422 on daemons predating M3 PATCH) · 2 usage
# Note: protocol-v1.md §4 lists GET /v1/config as a "read-only projection (M1)";
# the PATCH route landed with M3 and is live on this build — wire reality wins.
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("VOXIX_BASE_URL", "http://127.0.0.1:47832").rstrip("/")
TOKEN = os.environ.get("VOXIX_TOKEN")
if not TOKEN:
    sys.exit("error: VOXIX_TOKEN is not set")


def call(method, path, body=None):
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except urllib.error.URLError as e:
        sys.exit(f"error: cannot reach {BASE} ({e.reason})")


print("--- GET /v1/config ---")
status, body = call("GET", "/v1/config")
try:
    config = json.loads(body)
except json.JSONDecodeError:
    sys.exit(f"GET failed: HTTP {status} {body}")
print(json.dumps(config, indent=2))
if status != 200:
    sys.exit(1)

# Extract current max_audio_seconds (KeyError if the shape ever changes — fail loudly).
current = config["max_audio_seconds"]
print(f"--- PATCH /v1/config {{\"max_audio_seconds\": {current}}}  (same-value no-op) ---")

# Note: PATCH is NOT idempotent for read-only builds of the daemon; if your
# build still rejects PATCH it answers 422 invalid_request and this step fails —
# that is the signal the daemon predates M3 config.
status, body = call("PATCH", "/v1/config", {"max_audio_seconds": current})
try:
    print(json.dumps(json.loads(body), indent=2))
except json.JSONDecodeError:
    print(body if body else "(empty body)")
if status != 200:
    sys.exit(1)
