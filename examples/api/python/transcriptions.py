#!/usr/bin/env python3
# transcriptions.py — batch (non-streaming) transcription over raw HTTP from Python.
#
# SDK twin: ../../sdk/04-batch.ts — same flow with the typed SDK client and the
# six-class error taxonomy. If you are already writing TypeScript, use that.
#
# Demonstrates:
#   POST   /v1/transcriptions        — raw WAV body (application/octet-stream), 64 MiB cap;
#                                      202 {job_id, state:"processing", status_url}
#                                      Rejections: 429 queue-full · 409 service_busy
#                                      (live session holds the inference lease) · 400 invalid-audio
#   GET    /v1/transcriptions/{job}  — poll; states: processing → completed|failed|cancelled
#                                      404 not_found for unknown ids
#   DELETE /v1/transcriptions/{job}  — best-effort cancel: 204 while processing,
#                                      409 job_settled once terminal, 404 unknown
#
# Wire fidelity notes:
#   * The body is the WAV file bytes verbatim — no multipart, no JSON.
#   * Error bodies always carry {"error":{"code","message","retryable","request_id","details?"}}.
#
# Runtime: python3, standard library only (urllib) — the original version of this
# script used `requests`; the api/ tree installs nothing beyond curl/python3.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   python3 examples/api/python/transcriptions.py ./speech.wav
#   python3 examples/api/python/transcriptions.py ./speech.wav --cancel-after-start
#
# Expected output: the 202 envelope, poll status lines, then the transcript
# (or a clear {error:{…}} failure).
# Exit codes: 0 completed · 1 failed/cancelled/timed out · 2 usage
import json
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("VOXIX_BASE_URL", "http://127.0.0.1:47832").rstrip("/")
TOKEN = os.environ.get("VOXIX_TOKEN")
args = sys.argv[1:]
flags = [a for a in args if a.startswith("--")]
pos = [a for a in args if not a.startswith("--")]
if not TOKEN:
    sys.exit("error: VOXIX_TOKEN is not set")
if len(pos) != 1 or any(f not in ("--cancel-after-start",) for f in flags):
    sys.exit("usage: transcriptions.py <file.wav> [--cancel-after-start]")
WAV = pos[0]
if not os.path.isfile(WAV):
    sys.exit(f"error: no such file: {WAV}")
AUTH = {"Authorization": f"Bearer {TOKEN}"}


def call(method, url, body=None, content_type=None):
    req = urllib.request.Request(url, data=body, method=method)
    for k, v in AUTH.items():
        req.add_header(k, v)
    if content_type:
        req.add_header("Content-Type", content_type)
    try:
        with urllib.request.urlopen(req, timeout=300) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        # Non-2xx still carries the {error:{code,message,retryable,…}} envelope.
        return e.code, e.read().decode("utf-8", "replace")
    except urllib.error.URLError as e:
        sys.exit(f"error: cannot reach {BASE} ({e.reason})")


def show(label, status, body):
    print(f"--- {label} → HTTP {status} ---")
    try:
        print(json.dumps(json.loads(body), indent=2))
    except json.JSONDecodeError:
        print(body if body else "(empty body)")


# --- POST /v1/transcriptions -------------------------------------------------
with open(WAV, "rb") as fh:
    wav = fh.read()
print(f"--- POST /v1/transcriptions ({len(wav)} bytes) ---")
status, body = call("POST", f"{BASE}/v1/transcriptions", body=wav,
                    content_type="application/octet-stream")
if status != 202:
    show("POST /v1/transcriptions", status, body)
    sys.exit(1)
accepted = json.loads(body)  # {job_id, state:"processing", status_url}
print(json.dumps(accepted, indent=2))

if "--cancel-after-start" in flags:
    # --- DELETE /v1/transcriptions/{job} — best-effort cancel (204 vs 409) ---
    status, body = call("DELETE", f"{BASE}/v1/transcriptions/{accepted['job_id']}")
    print(f"DELETE → HTTP {status} (204 = cancelled while processing, 409 = already settled, 404 = unknown)")
    sys.exit(0 if status in (204, 409) else 1)

# --- GET /v1/transcriptions/{job} — poll loop --------------------------------
print(f"--- GET /v1/transcriptions/{accepted['job_id']} (polling) ---")
deadline = time.time() + 300
while True:
    status, body = call("GET", f"{BASE}/v1/transcriptions/{accepted['job_id']}")
    if status != 200:
        show("GET /v1/transcriptions/{job}", status, body)
        sys.exit(1)
    s = json.loads(body)
    state = s.get("state")
    if state == "completed":
        show("job completed", status, body)
        print("--- transcript ---")
        print(s["result"]["transcript"])
        sys.exit(0)
    if state in ("failed", "cancelled"):
        show("job ended", status, body)
        err = s.get("error") or {}
        print(f"job {state}: {err.get('code', '?')} {err.get('message', '')}", file=sys.stderr)
        sys.exit(1)
    if time.time() > deadline:
        print(f"timed out after 300 s (job is still processing; retry or DELETE {accepted['job_id']})", file=sys.stderr)
        sys.exit(1)
    print(f"  … {state}")
    time.sleep(0.25)
