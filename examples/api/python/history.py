#!/usr/bin/env python3
# history.py — paginated transcript history.
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
# PRIVACY: this prints your private transcripts to the terminal. Only run it on
# a machine you trust, and don't pipe the output somewhere you wouldn't paste
# your own dictations.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   python3 examples/api/python/history.py [LIMIT]
#
# Expected output: a compact table of the newest entries (id, time, source, title).
# Exit codes: 0 ok (including an empty history) · 1 request/503/404 failure · 2 usage
import json
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("VOXIX_BASE_URL", "http://127.0.0.1:47832").rstrip("/")
TOKEN = os.environ.get("VOXIX_TOKEN")
LIMIT = sys.argv[1] if len(sys.argv) > 1 else "5"
if not TOKEN:
    sys.exit("error: VOXIX_TOKEN is not set")
if not LIMIT.isdigit():
    sys.exit("usage: history.py [LIMIT]  (LIMIT must be an integer; clamps to 1–100)")


def call(method, path):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except urllib.error.URLError as e:
        sys.exit(f"error: cannot reach {BASE} ({e.reason})")


print(f"--- GET /v1/history?limit={LIMIT} ---")
status, body = call("GET", f"/v1/history?limit={LIMIT}")
try:
    page = json.loads(body)
except json.JSONDecodeError:
    sys.exit(f"GET failed: HTTP {status} {body}")
if status != 200:
    print(json.dumps(page, indent=2), file=sys.stderr)
    sys.exit(1)
print(json.dumps(page, indent=2))

entries = page.get("entries", [])
if not entries:
    print("(empty — nothing recorded yet; run a batch job or a stream with save_history on)")
else:
    print()
    print(f"{'ID':>6}  {'WHEN':<19}  {'SRC':<6}  TITLE")
    for e in entries:
        when = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(e.get("timestamp_ms", 0) / 1000))
        print(f"{e.get('id', '?'):>6}  {when:<19}  {str(e.get('source', '?')):<6}  {e.get('title', '')[:60]}")
    if page.get("has_more"):
        print(f"(more entries — pass cursor={page.get('next_cursor')} to walk older pages)")
