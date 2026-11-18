#!/usr/bin/env python3
# models.py — the model catalog surface.
#
# Demonstrates:
#   GET  /v1/models?installed=true   — list installed models only
#                                      (wire shape is `installed=true|false`, NOT filter[…];
#                                       omit the query param to list everything)
#   GET  /v1/models/{id}             — one model + prepare-stage projection
#                                      (ids may contain slashes, e.g. ggml/whisper — the
#                                       wildcard route decodes percent-escapes once)
#   GET  /v1/models/{id}/downloads   — managed-download status projection
#                                      {state,downloaded_bytes,total_bytes,percentage?}
#   PUT  /v1/models/selected         — select + eagerly load a model  [--select MODEL_ID]
#                                      body: {"model_id": "…", "load": "eager"|"lazy"}
#                                      ("eager" default; PUT — not POST)
#                                      409 model_not_ready when not installed locally —
#                                      the service never implicitly downloads
#
# DEFAULT IS PRINT-ONLY: running this script never changes daemon state. The PUT
# selection step loads the model (it holds the inference lease), so it is gated
# behind `--select MODEL_ID`. Do not pass it unless you mean to switch the active model.
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   python3 examples/api/python/models.py                # list + show first installed model
#   python3 examples/api/python/models.py ggml/whisper   # show a specific model id
#   python3 examples/api/python/models.py --select ID    # ALSO PUT the selection (mutates!)
#
# Expected output: models list, one-model projection, download status.
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
select = None
if "--select" in args:
    i = args.index("--select")
    try:
        select = args[i + 1]
    except IndexError:
        sys.exit("usage: models.py [MODEL_ID] [--select MODEL_ID]")
    del args[i:i + 2]
model_id_arg = args[0] if args else None


def call(method, path, body=None):
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
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
    return status


print("--- GET /v1/models?installed=true ---")
status, body = call("GET", "/v1/models?installed=true")
try:
    models_doc = json.loads(body)
except json.JSONDecodeError:
    sys.exit(f"GET failed: HTTP {status} {body}")
print(json.dumps(models_doc, indent=2))
if status != 200:
    sys.exit(1)

models = models_doc.get("models", [])
model_id = model_id_arg or (select if select else (models[0]["id"] if models else None))
if not model_id:
    sys.exit("error: no installed models found and no MODEL_ID argument given\n"
             "       (the daemon never implicitly downloads; install a model in the app first)")
print(f"(using MODEL_ID={model_id})")

# Model ids may contain `/`; urllib passes the path verbatim (the server decodes once).
status = show(f"GET /v1/models/{model_id}", *call("GET", f"/v1/models/{model_id}"))
if status != 200:
    sys.exit(1)
show(f"GET /v1/models/{model_id}/downloads", *call("GET", f"/v1/models/{model_id}/downloads"))

if select:
    # PUT /v1/models/selected MUTATES daemon state (it eagerly loads the model on this
    # build, and 409s during an inference lease). Explicitly requested via --select.
    print(f"--- PUT /v1/models/selected {{\"model_id\": \"{select}\", \"load\": \"eager\"}} ---")
    status = show("PUT /v1/models/selected", *call("PUT", "/v1/models/selected", {"model_id": select, "load": "eager"}))
    sys.exit(0 if status == 200 else 1)
else:
    print("(skipping PUT /v1/models/selected — pass --select MODEL_ID to run it; it loads the model)")
