#!/usr/bin/env python3
# sessions.py — the session lifecycle over raw HTTP (create → get → complete → delete).
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
# Wire notes:
#   * Session routes require BOTH the bearer AND the per-session X-Session-Token
#     returned once in the 201 body. The token is a 60 s-scoped credential for the
#     WS upgrade (and this REST surface); treat it like a password.
#   * The WS media plane (audio in, partials/finals out) is NOT usable with plain
#     urllib — it needs a WebSocket client speaking the protocol-v1 frame discipline
#     . See sessions_stream.py for the working path.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   python3 examples/api/python/sessions.py
#
# Expected output: the 422 envelope from the unknown-field probe (a no-op —
# nothing is allocated), then the 201 envelope, the GET projection, the PATCH
# result, and the two idempotent DELETEs.
# Exit codes: 0 ok · 1 request failure · 2 usage
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("VOXIX_BASE_URL", "http://127.0.0.1:47832").rstrip("/")
TOKEN = os.environ.get("VOXIX_TOKEN")
if not TOKEN:
    sys.exit("error: VOXIX_TOKEN is not set")


def call(method, path, body=None, session_token=None):
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    if session_token is not None:
        req.add_header("X-Session-Token", session_token)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except urllib.error.URLError as e:
        sys.exit(f"error: cannot reach {BASE} ({e.reason})")


def show(label, status, body):
    print(f"--- {label} → HTTP {status} ---")
    try:
        print(json.dumps(json.loads(body), indent=2))
    except json.JSONDecodeError:
        print(body if body else "(empty body)")


# --- POST /v1/sessions with an UNKNOWN field — 422, nothing is allocated -------
print('--- POST /v1/sessions {"model_idd": "typo"} (unknown field → 422) ---')
status, body = call("POST", "/v1/sessions", {"model_idd": "typo"})
show("POST /v1/sessions (unknown field)", status, body)
if status != 422:
    print(f"note: expected 422 for unknown field, got {status} — is this an M3+ build?", file=sys.stderr)

# --- POST /v1/sessions — the real create -------------------------------------
print("--- POST /v1/sessions ---")
status, body = call("POST", "/v1/sessions", {})
if status != 201:
    show("POST /v1/sessions", status, body)
    sys.exit(1)
created = json.loads(body)
show("POST /v1/sessions", status, body)
session_id = created["session_id"]
session_token = created["session_token"]  # shown once; the projection never carries it

# --- GET /v1/sessions/{id} ---------------------------------------------------
status, body = call("GET", f"/v1/sessions/{session_id}", session_token=session_token)
show("GET /v1/sessions/{id} (echo before PATCH)", status, body)
if status != 200:
    sys.exit(1)

# --- PATCH /v1/sessions/{id} {"state":"completed"} → 200 projection ----------
status, body = call("PATCH", f"/v1/sessions/{session_id}", {"state": "completed"}, session_token=session_token)
show('PATCH /v1/sessions/{id} {"state":"completed"}', status, body)
if status != 200:
    sys.exit(1)

# --- DELETE /v1/sessions/{id} — idempotent cancel, twice ----------------------
for attempt in (1, 2):
    status, body = call("DELETE", f"/v1/sessions/{session_id}", session_token=session_token)
    print(f"--- DELETE /v1/sessions/{{id}} (attempt {attempt}) → HTTP {status} (204 expected, idempotent) ---")
    if status != 204:
        print(body, file=sys.stderr)
        sys.exit(1)
print("done — session completed and delete is idempotent")
