#!/usr/bin/env python3
# ws_ticket.py — mint a one-use ticket for the WebSocket upgrade.
#
# Demonstrates:
#   POST /v1/auth/ws-ticket  {"session_id"?} → 201
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
# The ticket is single-use and bearer-equivalent for 60 s — it is printed only
# with --print-token. Otherwise only a redacted prefix is shown.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   python3 examples/api/python/ws_ticket.py                # unbound ticket (redacted)
#   python3 examples/api/python/ws_ticket.py <session-id>   # bound to that session
#   python3 examples/api/python/ws_ticket.py --print-token  # actually print the ticket
#
# Expected output: the 201 status and redacted ticket metadata (or the ticket itself).
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

args = sys.argv[1:]
print_token = "--print-token" in args
args = [a for a in args if a != "--print-token"]
session_id = args[0] if args else None
if len(args) > 1:
    sys.exit("usage: ws_ticket.py [SESSION_ID] [--print-token]")

body = {"session_id": session_id} if session_id else {}
data = json.dumps(body).encode("utf-8")
req = urllib.request.Request(BASE + "/v1/auth/ws-ticket", data=data, method="POST")
req.add_header("Authorization", f"Bearer {TOKEN}")
req.add_header("Content-Type", "application/json")
try:
    with urllib.request.urlopen(req, timeout=10) as resp:
        status, payload = resp.status, resp.read().decode("utf-8", "replace")
except urllib.error.HTTPError as e:
    status, payload = e.code, e.read().decode("utf-8", "replace")
except urllib.error.URLError as e:
    sys.exit(f"error: cannot reach {BASE} ({e.reason})")

print(f"--- POST /v1/auth/ws-ticket → HTTP {status} (201 expected) ---")
try:
    doc = json.loads(payload)
except json.JSONDecodeError:
    print(payload if payload else "(empty body)")
    sys.exit(1 if status != 201 else 0)

if status != 201:
    print(json.dumps(doc, indent=2))
    sys.exit(1)

ticket = doc.get("ticket", "")
if print_token:
    print(json.dumps(doc, indent=2))
else:
    redacted = dict(doc)
    redacted["ticket"] = f"{ticket[:6]}… (redacted — pass --print-token to reveal)"
    print(json.dumps(redacted, indent=2))
print("(note: the ticket is single-use and dies in 60 s — do not retry it)")
