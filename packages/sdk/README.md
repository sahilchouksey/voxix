<p align="center">
  <img src="../../core/icons/logo.png" alt="voxix logo" width="72" />
</p>

# @voxix/sdk — voxix speech-service SDK (protocol v1)

TypeScript SDK for the self-hosted [voxix speech service](../..) over its
HTTP/1.1 control plane and WebSocket media plane, protocol v1. ESM-only, zero runtime dependencies, declarations
shipped; Node ≥ 20, Bun, browsers, and Electron/RN-shaped runtimes via
transport injection.

- **Core (this entry)** — `VoxixClient` (REST), `VoxixSession` (WS media
  plane), the six tagged error classes, the frame codec, and the bounded
  single-consumer observation tap. No DOM imports; both transports are
  injectable.
- **`@voxix/sdk/web`** — browser capture helpers (getUserMedia →
  AudioWorklet → 16 kHz PCM16).
- **`@voxix/sdk/node`** — WAV reading helpers and the embedded-child
  `create()` launcher (`voxix serve --embed`).

Wire fidelity: payloads are declared snake_case exactly as the service
speaks them — the SDK performs no case transformation, and unknown wire
keys are ignored (additive evolution only).

## Quickstart (stream)

Start the daemon once and note the token it prints once:

```bash
./voxix serve --bind 0.0.0.0:47832
# First start generated vx_… bearer token (printed once). Re-distribution of
# the plaintext is your pairing flow's job — the daemon stores only the
# SHA-256 verifier.
```

Stream microphone audio (web) and print finals:

```ts
import { VoxixClient } from "@voxix/sdk";

const client = new VoxixClient({
  baseUrl: "http://localhost:47832",
  token: secureStoredToken, // string or () => Promise<string>
});

const session = await client.createSession({
  vad: { mode: "server" }, // §3.4: server | client | disabled
  partial_results: true,
});

await session.connect(); // hello barrier
await session.start(); // started barrier (carries the resume pair)

// Observation tap — bounded, drop-oldest, single consumer (second
// concurrent consumer errors strictly):
void (async () => {
  for await (const event of session.events) {
    if (event.type === "partial")
      renderDraft(event.payload.committed + event.payload.tentative);
    if (event.type === "final") renderFinal(event.payload.text);
    if (event.type === "error") handleError(event.payload.code);
  }
})();

// Push §6 binary frames (20 ms = 320 samples at 16 kHz mono):
for await (const frame of capturePcm16Frames()) {
  // your own capture
  await session.pushAudio(frame.samples, 20);
}

const final = await session.stop("user"); // FIFO barrier → one final
session.close();
```

The web capture part (getUserMedia → AudioWorklet → resampler → PCM16
frames) is in `@voxix/sdk/web` — `MicrophonePcm16` yields frames that map
1:1 onto `session.pushAudio`; `examples/web-capture.ts` shows the full loop.

## REST surface (VoxixClient)

| Method                                  | Route                                                                                                                                                            |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `alive(signal?)`                        | `GET /alive` (no auth)                                                                                                                                           |
| `health(signal?)`                       | `GET /v1/health`                                                                                                                                                 |
| `status(signal?)`                       | `GET /v1/status`                                                                                                                                                 |
| `listModels({installed?}?, signal?)`    | `GET /v1/models?installed=`                                                                                                                                      |
| `getModel(id)`                          | `GET /v1/models/{id}` (`{model, prepare}`)                                                                                                                       |
| `getDownload(id)`                       | `GET /v1/models/{id}/downloads`                                                                                                                                  |
| `selectModel(id, load?)`                | `PUT /v1/models/selected` (`eager`/`lazy`)                                                                                                                       |
| `prepareModel(id, onProgress?)`         | `POST /v1/models/{id}/prepare` — drives the SSE progress stream to a terminal state (progress callback optional)                                                 |
| `downloadModel(id)`                     | `POST /v1/models/{id}/download` — starts/converges on a managed download; `200` already installed, `202` accepted/in flight; poll `getDownload(id)` for progress |
| `cancelModelDownload(id)`               | `DELETE /v1/models/{id}/download` — cancels an in-flight managed download (`204`); partials kept for resume                                                      |
| `prepareEvents(id)`                     | the same SSE stream as an async iterable (additive power method)                                                                                                 |
| `createSession(body, signal?)`          | `POST /v1/sessions` → VoxixSession                                                                                                                               |
| `attachSession(created)`                | rebind an existing created-session object (resume flows)                                                                                                         |
| `getSession(id, sessionToken)`          | `GET /v1/sessions/{id}` (per-session header)                                                                                                                     |
| `transcribeFile(blobOrBuffer, signal?)` | `POST /v1/transcriptions` (raw WAV bytes) → 202 job                                                                                                              |
| `getTranscription(jobId)`               | `GET /v1/transcriptions/{job}`                                                                                                                                   |
| `cancelTranscription(jobId)`            | `DELETE /v1/transcriptions/{job}` (false on 404/409)                                                                                                             |
| `history({cursor?, limit?}?, signal?)`  | `GET /v1/history`                                                                                                                                                |
| `config() / patchConfig(patch)`         | `GET/PATCH /v1/config`                                                                                                                                           |
| `mintTicket(sessionId?)`                | `POST /v1/auth/ws-ticket` (one-use 60 s, additive)                                                                                                               |

Batch polling helper: `transcribeFile` resolves at 202; poll
`getTranscription` until `completed` / `failed` / `cancelled`.

## Session surface (VoxixSession)

```
connect(signal?)  → waits for hello            | phases: new → connecting → connected
start(opts?)      → waits for started          | connected → starting → streaming
pushAudio(frame, durationMs)                  | Int16Array or LE Uint8Array, 10/20/30/40/60 ms
pause()            → resolves on the paused payload | streaming → paused
resume()           → wire resume + tap `resumed` (client-synthesized) | paused → streaming
stop(reason?)      → resolves with FinalResult (exactly one) | streaming → stopped
cancel()           → results suppressed; no final ever; resolves on close | → cancelled
close()            → local teardown (idempotent) | any → closed
```

- `events` is the tap for every server observation (`hello`, `started`,
  `paused`, `speech_start`, `speech_end`, `audio_ack`, `partial`,
  `finalizing`, `final`, `error`). `resumed` is client-synthesized on
  `resume()`; `pong` is consumed internally and never surfaced.
- `pushAudio` places the frame on the wire synchronously; bounded-buffer
  backpressure (via `bufferedAmount` when the socket exposes it) happens
  strictly after, preserving call-order FIFO for the server's stop/pause
  barriers.
- `session.resumeCredentials` after `started` — `{resume_token,
resume_epoch}`; `start({resume: pair})` echoes them. A mismatched/stale
  pair is the terminal `stale_resume` error + close 4003.
- Handshake timeouts are per-client options (`timeouts`: hello 10 s,
  started 30 s, final 60 s, close grace 5 s, app-level ping every 5 s).

## Errors — six tagged classes (§15 G4)

`VoxixError` carries `code` (`transport` / `auth` / `protocol` / `service`
/ `model` / `cancelled`), optional `status`, `remoteCode` (the server's
`error.code` verbatim), `retryable`, and `cause`.

| Class            | Covers                                                                                                                       |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `TransportError` | fetch/WS failures, daemon shutdown (1001)                                                                                    |
| `AuthError`      | 401/403 responses, rotated bearer (close 4001)                                                                               |
| `ProtocolError`  | malformed frames/versions, close codes 1008/4002/4004, second-consumer taps, surface misuse                                  |
| `ServiceError`   | REST envelope errors, media-plane error events (`stale_resume`, lease contention `service_busy`, engine 1011, overload 1013) |
| `ModelError`     | `model_not_found`, `model_not_ready`, `prepare-busy`                                                                         |
| `CancelledError` | aborts, `close()`, explicit cancel                                                                                           |

WS close-code mapping follows the service's carrier table (1000 without a
terminal event → `TransportError`; 4003 → `ServiceError remoteCode
"stale_resume"`; …).

## Tunings

- Taps are bounded (512 events) and drop-oldest: lagging consumers lose
  old partial revisions instead of memory. Iterate once.
- `pushAudio` waits only when the socket reports `bufferedAmount` above
  512 KiB (high water 512 KiB / low water 128 KiB).
- `timeouts` are options on the client; long model loads should raise
  `startedMs`.

## Transports

```ts
new VoxixClient({
  baseUrl,
  token,
  fetch: myFetch, // REST/batch/SSE (defaults: globalThis.fetch)
  webSocketFactory: (url, { headers }) => myWs, // media plane
  ticketFallback: true, // mint ?ticket= instead of the bearer header
});
```

- Browsers/Node ≥22 (undici) WebSockets cannot attach custom headers. The
  media plane upgrades require `Authorization`/`X-Session-Token`: use
  `webSocketFactory` with a header-capable adapter in those runtimes, or
  set `ticketFallback: true` (mints a one-use `?ticket=` per session; the
  `X-Session-Token` scope stays mandatory — that sub-credential is the
  server's anti-probe design).
- Bun's WebSocket honors `{ headers }` — the default factory works there.
- Node adapters typically wrap the `ws` package (which accepts `headers`).

## Embedded child (`@voxix/sdk/node`)

```ts
import { create } from "@voxix/sdk/node";

const client = await create({
  command: "voxix", // spawned as: voxix serve --embed
  model: "en-tiny-16k", // resolves only after prepare completes (SSE)
  onPrepare: (event) => console.log(event.event),
});
console.log(await client.health());
await client.dispose(); // stdin EOF is the child's lifetime lease
```

`create()` validates the single stdout ready line (loopback http URL,
`vx_`-prefixed token, positive-integer pid cross-checked against the
spawned child, api version, byte cap), keeps stdin open as the lease, and
retains the stderr head for spawn-failure diagnosis. `create({model})` is
the model-bound ready constructor: a failed or cancelled creation closes
the child before rejecting; `dispose()` is idempotent.

## Examples and tests

- `examples/node-transcribe.ts` — runnable CLI (WAV → `pushAudio` loop).
- `examples/web-capture.ts` — browser capture mapping.
- `examples/android-okhttp/` — Android/OkHttp pointer (reference-only).
- `test/` — offline suite over the bundled fake daemon (REST + WS), real
  fetch/WebSocket, no network stubs. Run with `bun test test/` (from the
  repository root: `bun run test:sdk`).

## License

MIT — see the repository root.
