# Android / OkHttp pointer example — REFERENCE-ONLY

`OkHttpTranscriber.kt` is **reference code**, not a shipped artifact: no
Gradle wiring, no published package, no compatibility claim. It documents
how an Android capture host speaks protocol v1 directly over OkHttp:

1. `POST /v1/sessions` with the bearer header → 201 body carries the
   `session_id`, the per-session `session_token` sub-credential, and the
   relative `ws_url`.
2. Open the media socket with **custom headers** — OkHttp supports this:
   `Authorization: Bearer vx_…` **and** `X-Session-Token: <session_token>`.
   (Browser/undici WebSockets cannot attach headers: runtimes in that
   position should mint a one-use ticket via `POST /v1/auth/ws-ticket` and
   append `?ticket=…` instead — the ticket substitutes the bearer only, the
   `X-Session-Token` scope stays mandatory.)
3. `{"type":"start","payload":{}}` — the `started` barrier precedes audio.
4. Stream `AudioRecord` reads sliced into 10/20/30/40/60 ms buffers at
   16 kHz mono PCM16 as **binary** frames using the §6 12-byte header
   (`VX`, version 1, codec 1, big-endian u32 sequence from 0, big-endian u16
   duration, big-endian u16 flags, little-endian payload).
5. `{"type":"ping","payload":{"nonce":N}}` refreshes pause/idle liveness.
6. Finish with `{"type":"stop","payload":{"reason":"user"}}` — the server
   emits exactly one `final` event, then closes 1000. `cancel` suppresses
   results instead.

Kotlin capture (AGC/noise-suppression presets) mirrors the SDK core's
contract: one inference lease per service (`max_concurrent_streams: 1`), so
the app patterns a client around admission (`503 service_full`,
`Retry-After`) and lease contention (`service_busy` is retryable).
