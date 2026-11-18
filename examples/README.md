<p align="center">
  <img src="../core/icons/logo.png" alt="voxix logo" width="72" />
</p>

# examples/ — voxix service + SDK capability tour

Every capability of the voxix speech service, exercised three ways:

- **`api/`** — raw HTTP with zero installable dependencies, in three flavors.
  Wire-level: exact routes, headers, envelopes.
  - **`api/curl/`** — POSIX `curl`; the canonical zero-dep reference.
  - **`api/python/`** — Python 3 standard library (`urllib`); only
    `sessions_stream.py` needs anything beyond stdlib (`websockets`).
  - **`api/javascript/`** — node ≥ 20 (global `fetch`); streaming needs
    node ≥ 22 (global `WebSocket`).
- **`sdk/`** — TypeScript against the in-repo SDK (`packages/sdk/src/`). Typed,
  with the six-class error taxonomy, the WS session lifecycle, and backpressure.

> **The SDK is not published to npm** — `@voxix/sdk` does not exist in any
> registry. Imports in `sdk/` resolve the in-repo source by _relative path_
> (e.g. `../../packages/sdk/src/client.js`). Do not `npm install @voxix/sdk`.

## Prerequisites

1. A running daemon (build once, then keep it running):

   ```bash
   ./voxix serve --bind 127.0.0.1:47832
   ```

   First start generates the bearer token and **prints it once** (stderr when
   interactive; stdout with `--token-stdout always`; never with `never`).
   The daemon stores only the SHA-256 verifier. The desktop app keeps its own
   copy at `~/Library/Application Support/dev.xix3r.voxix/state/gui-client.json`
   (0600, plaintext — the GUI's own credential, do not redistribute it).

2. Export it (every example reads it; nothing prompts):

   ```bash
   export VOXIX_TOKEN=vx_…
   export VOXIX_BASE_URL=http://127.0.0.1:47832   # optional; this is the default
   ```

   Bun for the TypeScript examples: `~/.bun/bin/bun` (not on PATH by default).
   `python3` for the Python examples (all stdlib except `sessions_stream.py`,
   which needs `pip install websockets` — v13+ verified). The JavaScript
   examples run on node ≥ 20; `sessions_stream.mjs` needs node ≥ 22 (global
   `WebSocket`). `node`/`npm` are not required for anything else.

## Auth gotchas (read before your first 401)

- **Every `/v1/*` route requires `Authorization: Bearer vx_…`** — except
  `/alive`, which is bare for supervisors/proxies. Rejection is **401 with an
  empty body** (by design: no probe enrichment).
- **Session routes additionally require the per-session `X-Session-Token`**
  sub-credential returned once at create time. Wrong/missing value → **404
  `session_not_found`**, never 403 — session ids cannot be probed.
- **WS upgrade** (`GET /v1/sessions/{id}/stream`) needs **both** the bearer
  (or a one-use `?ticket=` minted at `POST /v1/auth/ws-ticket`) **and**
  `X-Session-Token`. A ticket substitutes the bearer only, never the
  session token, and only while valid, unexpired and unconsumed.
- Error bodies always carry the envelope
  `{"error":{"code","message","retryable","request_id","details?"}}`;
  `Retry-After` carries the hint on 503s.

## Contents

**curl** (`api/curl/`) — the canonical zero-dep reference:
| `api/curl/health.sh` | `/alive` (unauthenticated), `/v1/health`, `/v1/status` (bearer) | `./examples/api/curl/health.sh` |
| `api/curl/config.sh` | `GET /v1/config`; same-value `PATCH /v1/config` (mutates on M3+ builds) | `./examples/api/curl/config.sh` |
| `api/curl/models.sh` | `GET /v1/models?installed=true`, `GET /v1/models/{id}`, `GET /v1/models/{id}/downloads`, optional `PUT /v1/models/selected` (SELECT=1) | `./examples/api/curl/models.sh [MODEL_ID]` |
| `api/curl/transcriptions.sh` | `POST /v1/transcriptions` (raw WAV) + poll + `DELETE` (`DELETE_AFTER_START=1`) | `./examples/api/curl/transcriptions.sh ./speech.wav` |
| `api/curl/sessions.sh` | `POST /v1/sessions` → `GET/PATCH/DELETE /v1/sessions/{id}` lifecycle | `./examples/api/curl/sessions.sh [completed\|cancelled\|delete]` |
| `api/curl/history.sh` | `GET /v1/history?limit=…` (+ cursor note) and `GET /v1/history/{id}` | `./examples/api/curl/history.sh [LIMIT] [ENTRY_ID]` |
| `api/curl/tokens.sh` | `POST /v1/auth/tokens` — **dry-run by default**; rotation cuts over _all_ clients | `./examples/api/curl/tokens.sh [NAME]` |
| `api/curl/ws-ticket.sh` | `POST /v1/auth/ws-ticket` — mint a one-use 60 s WS-upgrade ticket (201) | `./examples/api/curl/ws-ticket.sh [SESSION_ID]` |

**python** (`api/python/`) — python3 stdlib only, except `sessions_stream.py`:

| File                            | Capability                                                                                                                                          | Run                                                                     |
| ------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| `api/python/health.py`          | `/alive`, `/v1/health`, `/v1/status`                                                                                                                | `python3 examples/api/python/health.py`                                 |
| `api/python/config.py`          | `GET /v1/config`; same-value `PATCH /v1/config` no-op round-trip                                                                                    | `python3 examples/api/python/config.py`                                 |
| `api/python/models.py`          | `GET /v1/models?installed=true`, `GET /v1/models/{id}`, `GET /v1/models/{id}/downloads`; `--select ID` gates the mutating `PUT /v1/models/selected` | `python3 examples/api/python/models.py [MODEL_ID] [--select ID]`        |
| `api/python/transcriptions.py`  | Raw WAV POST → 202 → poll → optional cancel (`--cancel-after-start`)                                                                                | `python3 examples/api/python/transcriptions.py ./speech.wav`            |
| `api/python/sessions.py`        | `POST /v1/sessions` → echo → `GET` → `PATCH completed` → idempotent `DELETE`; unknown-field 422 demo (prints only)                                  | `python3 examples/api/python/sessions.py`                               |
| `api/python/sessions_stream.py` | **WS streaming**: create → WS upgrade → `start` → 20 ms PCM16 frames → `stop("user")` → exactly one `final`. Needs `pip install websockets`         | `python3 examples/api/python/sessions_stream.py ./speech.wav`           |
| `api/python/history.py`         | `GET /v1/history?limit=5` (+ cursor note), compact table; prints private transcripts                                                                | `python3 examples/api/python/history.py [LIMIT]`                        |
| `api/python/ws_ticket.py`       | `POST /v1/auth/ws-ticket` → 201; ticket printed only with `--print-token`                                                                           | `python3 examples/api/python/ws_ticket.py [SESSION_ID] [--print-token]` |

**javascript** (`api/javascript/`) — node ≥ 20 (global `fetch`); streaming needs node ≥ 22:

| File                                 | Capability                                                                                                                                          | Run                                                                       |
| ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `api/javascript/health.mjs`          | `/alive`, `/v1/health`, `/v1/status`                                                                                                                | `node examples/api/javascript/health.mjs`                                 |
| `api/javascript/config.mjs`          | `GET /v1/config`; same-value `PATCH /v1/config` no-op round-trip                                                                                    | `node examples/api/javascript/config.mjs`                                 |
| `api/javascript/models.mjs`          | `GET /v1/models?installed=true`, `GET /v1/models/{id}`, `GET /v1/models/{id}/downloads`; `--select ID` gates the mutating `PUT /v1/models/selected` | `node examples/api/javascript/models.mjs [MODEL_ID] [--select ID]`        |
| `api/javascript/transcriptions.mjs`  | Raw WAV POST → 202 → poll → optional cancel (`--cancel-after-start`)                                                                                | `node examples/api/javascript/transcriptions.mjs ./speech.wav`            |
| `api/javascript/sessions.mjs`        | `POST /v1/sessions` → echo → `GET` → `PATCH completed` → idempotent `DELETE`; unknown-field 422 demo (prints only)                                  | `node examples/api/javascript/sessions.mjs`                               |
| `api/javascript/sessions_stream.mjs` | **WS streaming**: create → WS upgrade (global `WebSocket`, node ≥ 22) → `start` → 20 ms frames → `stop("user")` → exactly one `final`               | `node examples/api/javascript/sessions_stream.mjs ./speech.wav`           |
| `api/javascript/history.mjs`         | `GET /v1/history?limit=5` (+ cursor note), compact table; prints private transcripts                                                                | `node examples/api/javascript/history.mjs [LIMIT]`                        |
| `api/javascript/ws-ticket.mjs`       | `POST /v1/auth/ws-ticket` → 201; ticket printed only with `--print-token`                                                                           | `node examples/api/javascript/ws-ticket.mjs [SESSION_ID] [--print-token]` |

> **The streaming examples (`sessions_stream.py` / `sessions_stream.mjs` /
> `sdk/05-streaming.ts`) run real inference on the daemon (CPU).** They are
> functional demos, not stubs — expect wall-clock time ≈ WAV length + decode
> time, on a machine with a model installed. Every other example is
> control-plane traffic only.

The shell examples use `set -euo pipefail` and `-f` fail on HTTP errors; the
python and javascript examples carry the same discipline (envelope-aware error
printing, no silent 4xx/5xx swallowing). All three `api/` trees read the token
from `VOXIX_TOKEN` (exit 2 if unset) and exit **0** ok · **1** service/transport
failure · **2** usage. An empty history or a 503 `history_unavailable` is
reported, not masked.

**sdk/** — TypeScript against the in-repo SDK (`packages/sdk/src/`):

| File                                | Capability                                                                                                                     | Run                                                            |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------- |
| `sdk/01-health-and-status.ts`       | `alive()`, `health()`, `status()` + capability block                                                                           | `bun examples/sdk/01-health-and-status.ts`                     |
| `sdk/02-config.ts`                  | `config()`, `patchConfig()` (same-value no-op patch)                                                                           | `bun examples/sdk/02-config.ts`                                |
| `sdk/03-models.ts`                  | `listModels`, `getModel`, `selectModel`, `downloadModel`/`getDownload`/`cancelModelDownload` (all mutating paths behind flags) | `bun examples/sdk/03-models.ts [--select ID] [--download ID]`  |
| `sdk/04-batch.ts`                   | `transcribeFile` → poll `getTranscription` (+ `cancelTranscription` behind `--cancel`)                                         | `bun examples/sdk/04-batch.ts ./speech.wav`                    |
| `sdk/05-streaming.ts`               | WS media plane: `createSession` → `connect` → `start` → 20 ms frames from a WAV → events → `stop("user")` → one `final`        | `bun examples/sdk/05-streaming.ts ./speech.wav`                |
| `sdk/06-history.ts`                 | `history({limit})` iteration, page shape, table print                                                                          | `bun examples/sdk/06-history.ts [LIMIT] [--all]`               |
| `sdk/07-embed-child.ts`             | `@voxix/sdk/node` `create()` — spawn `voxix serve --embed`, health-check, dispose. Skips cleanly if `voxix` is not on PATH     | `bun examples/sdk/07-embed-child.ts`                           |
| `sdk/99-meeting-notes-assistant.ts` | Capstone: folder of interview WAVs → `meeting-notes.md` (preflight → catalog → batch loop → history → report)                  | `bun examples/sdk/99-meeting-notes-assistant.ts ./interviews/` |

## What goes where

| Capability               | `api/curl`              | `api/python`            | `api/javascript`         | `sdk/`                  | Notes                                                                                                                                                     |
| ------------------------ | ----------------------- | ----------------------- | ------------------------ | ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Liveness `/alive`        | `health.sh`             | `health.py`             | `health.mjs`             | `01`                    | no auth anywhere                                                                                                                                          |
| Health/status            | `health.sh`             | `health.py`             | `health.mjs`             | `01`                    |                                                                                                                                                           |
| Config read/patch        | `config.sh`             | `config.py`             | `config.mjs`             | `02`                    | PATCH is live on M3+ builds (docs said "read-only M1" — wire reality wins)                                                                                |
| Model catalog            | `models.sh`             | `models.py`             | `models.mjs`             | `03`                    | list query key is `?installed=true`, not `filter[installed]`; selection is **PUT**, body `{model_id, load?}`                                              |
| Managed downloads        | `models.sh` (poll only) | `models.py` (poll only) | `models.mjs` (poll only) | `03` (`--download`)     | start/cancel mutate state; kept behind flags                                                                                                              |
| Batch transcribe         | `transcriptions.sh`     | `transcriptions.py`     | `transcriptions.mjs`     | `04`                    | raw WAV body, 202 + job, poll, DELETE cancel                                                                                                              |
| Session lifecycle (REST) | `sessions.sh`           | `sessions.py`           | `sessions.mjs`           | (via SDK inside `05`)   | REST create/get/patch/delete only; 422 on unknown fields                                                                                                  |
| **WS streaming**         | —                       | `sessions_stream.py`    | `sessions_stream.mjs`    | `05-streaming.ts`       | curl cannot: binary frames + protocol discipline. python needs `pip install websockets`; javascript needs node ≥ 22 (global `WebSocket`) — see note below |
| WS ticket mint           | `ws-ticket.sh`          | `ws_ticket.py`          | `ws-ticket.mjs`          | (`mintTicket()` in SDK) | 201; single-use, 60 s                                                                                                                                     |
| History                  | `history.sh`            | `history.py`            | `history.mjs`            | `06`                    | may answer 503 `history_unavailable`                                                                                                                      |
| Token mint/rotate        | `tokens.sh`             | —                       | —                        | — (spec M4+)            | dry-run by default; rotation invalidates **all** paired tokens + drops live WS (close 4001)                                                               |
| Embedded child           | —                       | —                       | —                        | `07`                    | `voxix serve --embed`; ready-line on stdout is the only stdout output                                                                                     |
| End-to-end               | —                       | —                       | —                        | `99`                    | capstone: only SDK APIs + node stdlib                                                                                                                     |

Streaming note: the wire path requires a WebSocket that can carry binary
frames (`VX` + version + codec + seq + duration + flags + i16le mono PCM) and
the JSON envelope discipline. `curl` cannot
do this, which is why `api/curl/` has no streaming example. The two raw-wire
equivalents — `api/python/sessions_stream.py` (python3 + the `websockets`
package, v13+ verified) and `api/javascript/sessions_stream.mjs` (node ≥ 22
global `WebSocket`) — speak the same hello → start → frames → `stop("user")` →
`final` protocol as `sdk/05-streaming.ts` and run **real inference on the
daemon (CPU)**: they are functional demos, not stubs. The SDK path remains
the maintained, typed default.

## More reading

- `packages/sdk/README.md` — full SDK reference (quickstart, error taxonomy, `events` tap semantics)
- `packages/sdk/examples/` — upstream CLI examples (this directory is the curated tour)

## Verifying your checkout

```bash
bash -n examples/api/curl/*.sh                          # syntax
for f in examples/api/python/*.py; do
  python3 -c "import sys; compile(open(sys.argv[1]).read(), sys.argv[1], 'exec')" "$f"
done                                                    # syntax (py_compile is broken on SIP-locked macOS — use compile())
bun build --target=node examples/api/javascript/*.mjs --outdir <tmp-dir>   # parse check, exits non-zero on syntax errors
bun build --target=node examples/sdk/*.ts --outdir <tmp-dir>               # parse + import-resolution check
```
