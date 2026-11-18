#!/usr/bin/env node
// sessions_stream.mjs — streaming transcription over the WS media plane.
//
// SDK twin: ../../sdk/05-streaming.ts — same flow with the typed SDK client.
// This file is the raw-wire equivalent for people who want to see the protocol
// by hand. It needs the global WebSocket from node ≥ 22.
//
// Demonstrates the hello → start → frames → stop("user") → exactly-one `final`
// discipline ( §11):
//   1. POST /v1/sessions {"vad":{"mode":"server"},"partial_results":true} → 201
//      (carry the one-time session_token)
//   2. WS upgrade GET /v1/sessions/{id}/stream with bearer + X-Session-Token
//      → server `hello` (barrier: no start before hello)
//   3. send {"type":"start","payload":{}} → server `started` (barrier: NO AUDIO BEFORE THIS)
//   4. push ~20 ms binary frames: "VX" + 0x01 + codec(1) + seq u32 BE + dur u16 BE
//      + flags u16 BE + i16le mono PCM; ~320 samples per frame at 16 kHz,
//      real-time paced so server-side VAD observes natural timing
//   5. send {"type":"stop","payload":{"reason":"user"}} (FIFO finalize barrier)
//      → exactly one terminal `final` (partials may arrive before it)
//
// This runs REAL INFERENCE on the daemon (CPU) — it is a functional demo, not a
// stub. Use a short 16 kHz mono 16-bit PCM WAV. A repo fixture that works:
//   core/crates/vad-rs/tests/fixtures/dots.wav
//
// Notes on the wire:
//   * node ≥ 22 exposes undici's WebSocket globally. Its constructor accepts the
//     object form `new WebSocket(url, { headers })` — an undici extension (not
//     available in browsers) — which is how the bearer and X-Session-Token are
//     attached to the upgrade. If your runtime lacks that, see sdk/05-streaming.ts.
//   * Client→server control messages are plain {type,payload} JSON; audio is binary.
//   * Server→client messages are the full envelope {v:1,type,session_id,seq,ts_ms,payload}.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/sessions_stream.mjs ./speech.wav
//
// Expected output: session banner, live partials on one line, then exactly one
//   "final:" line with the transcript.
// Exit codes: 0 final received · 1 transport/protocol/service failure · 2 usage
import { readFile } from "node:fs/promises";

if (typeof globalThis.WebSocket !== "function") {
  console.error(
    "error: requires node ≥22 global WebSocket; see sdk/05-streaming.ts and protocol §11",
  );
  process.exit(1);
}

const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(
  /\/+$/,
  "",
);
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}
const wavPath = process.argv[2];
if (!wavPath) {
  console.error(
    "usage: sessions_stream.mjs <file.wav>   (16 kHz mono 16-bit PCM)",
  );
  process.exit(2);
}

const SAMPLE_RATE = 16_000;
const FRAME_MS = 20;
const SAMPLES_PER_FRAME = (SAMPLE_RATE * FRAME_MS) / 1000; // 320

const wsUrl = (b, p) =>
  b.replace(/^http:\/\//, "ws://").replace(/^https:\/\//, "wss://") + p;

// Minimal RIFF/WAVE parser — just enough to enforce the v1 audio profile.
const parseWav = (buf) => {
  if (
    buf.length < 12 ||
    buf.toString("ascii", 0, 4) !== "RIFF" ||
    buf.toString("ascii", 8, 12) !== "WAVE"
  ) {
    throw new Error("not a RIFF/WAVE file");
  }
  let off = 12;
  let fmt = null;
  let data = null;
  while (off + 8 <= buf.length) {
    const id = buf.toString("ascii", off, off + 4);
    const size = buf.readUInt32LE(off + 4);
    const body = off + 8;
    if (body + size > buf.length) break;
    if (id === "fmt ") {
      fmt = {
        audioFormat: buf.readUInt16LE(body),
        channels: buf.readUInt16LE(body + 2),
        sampleRate: buf.readUInt32LE(body + 4),
        bitsPerSample: buf.readUInt16LE(body + 14),
      };
    } else if (id === "data") {
      data = buf.subarray(body, body + size);
    }
    off = body + size + (size % 2); // chunks are word-aligned
  }
  if (!fmt || !data) throw new Error("missing fmt or data chunk");
  if (fmt.audioFormat !== 1)
    throw new Error(
      `audioFormat ${fmt.audioFormat} — only PCM (1) is supported; convert first`,
    );
  if (fmt.sampleRate !== SAMPLE_RATE)
    throw new Error(
      `input is ${fmt.sampleRate} Hz — the v1 audio profile is 16 kHz mono s16le; resample first`,
    );
  if (fmt.channels !== 1)
    throw new Error(
      `input has ${fmt.channels} channels — the v1 audio profile is mono; downmix first`,
    );
  if (fmt.bitsPerSample !== 16)
    throw new Error(
      `input is ${fmt.bitsPerSample}-bit — the v1 audio profile is 16-bit signed PCM; convert first`,
    );
  return data; // little-endian i16 samples, mono
};

const encodeFrame = (seq, pcm) => {
  const out = Buffer.alloc(12 + pcm.length);
  out.write("VX", 0, "ascii");
  out.writeUInt8(1, 2); // version 1
  out.writeUInt8(1, 3); // codec 1 = pcm_s16le
  out.writeUInt32BE(seq >>> 0, 4); // u32 BE audio sequence, starts at 0
  out.writeUInt16BE(FRAME_MS, 8); // u16 BE duration
  out.writeUInt16BE(0, 10); // u16 BE flags (bit 0 = discontinuity; unset)
  pcm.copy(out, 12);
  return out;
};

const httpJson = async (method, path, body) => {
  const init = { method, headers: { Authorization: `Bearer ${token}` } };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  const res = await fetch(base + path, init);
  return [res.status, await res.text()];
};

let pcm;
try {
  pcm = parseWav(await readFile(wavPath));
} catch (e) {
  console.error(`error: ${e.message}`);
  process.exit(2);
}
console.log(
  `decoded ${pcm.length / 2} samples (${Math.round(pcm.length / 2 / SAMPLE_RATE)} ms) from ${wavPath}`,
);

// --- create ------------------------------------------------------------------
// Server-side VAD auto-segments speech; partial_results surfaces `partial` events
// (final_only models emit exactly one `final`).
let [status, body] = await httpJson("POST", "/v1/sessions", {
  vad: { mode: "server" },
  partial_results: true,
});
if (status !== 201) {
  console.error(`create failed: HTTP ${status} ${body}`);
  process.exit(1);
}
const created = JSON.parse(body);
const sessionId = created.session_id;
const sessionToken = created.session_token;
console.log(
  `session ${sessionId} · streaming_mode=${created.streaming_mode} · negotiated=${JSON.stringify(created.negotiated_audio)}`,
);

// --- message pump with an internal queue --------------------------------------
const pending = [];
const waiters = [];
const pushMessage = (msg) => {
  if (waiters.length) waiters.shift()(msg);
  else pending.push(msg);
};
const nextMessage = (timeoutMs, what) =>
  new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`${what} timed out after ${timeoutMs} ms`)),
      timeoutMs,
    );
    const take = (msg) => {
      clearTimeout(timer);
      resolve(msg);
    };
    if (pending.length) return take(pending.shift());
    waiters.push(take);
  });

const finals = [];
const url = wsUrl(base, created.ws_url);

let socket;
try {
  // undici extension: object form supplies handshake headers (browsers cannot).
  socket = new WebSocket(url, {
    headers: {
      Authorization: `Bearer ${token}`,
      "X-Session-Token": sessionToken,
    },
  });
} catch (e) {
  console.error(`error: cannot open WebSocket (${e.message})`);
  console.error(
    "requires node ≥22 global WebSocket; see sdk/05-streaming.ts and protocol §11",
  );
  process.exit(1);
}

socket.addEventListener("message", (ev) =>
  pushMessage(typeof ev.data === "string" ? ev.data : null),
);
socket.addEventListener("close", (ev) => pushMessage(null));
socket.addEventListener("error", () => pushMessage(null));

const opened = new Promise((resolve, reject) => {
  socket.addEventListener("open", resolve, { once: true });
  socket.addEventListener(
    "error",
    () => reject(new Error("WebSocket upgrade failed")),
    { once: true },
  );
});

try {
  await opened;
  // hello barrier — the server speaks first; no start before hello.
  const hello = JSON.parse(await nextMessage(10_000, "hello"));
  if (hello?.type !== "hello")
    throw new Error(`protocol error: expected hello, got ${hello?.type}`);
  console.log(
    `hello · protocol_version=${hello.payload?.protocol_version} heartbeat_ms=${hello.payload?.heartbeat_ms}`,
  );
  // started barrier — NO AUDIO BEFORE THIS LINE.
  socket.send(JSON.stringify({ type: "start", payload: {} }));
  const started = JSON.parse(await nextMessage(10_000, "started"));
  if (started?.type !== "started")
    throw new Error(
      `protocol error: expected started, got ${started?.type}: ${JSON.stringify(started)}`,
    );
  console.log(
    `started · model_id=${started.payload?.model_id} backend=${started.payload?.backend}`,
  );

  // Pump frames: zero-padded tail, real-time pacing (~20 ms per frame).
  const frameBytes = SAMPLES_PER_FRAME * 2;
  let seq = 0;
  for (let off = 0; off < pcm.length; off += frameBytes) {
    let chunk = pcm.subarray(off, off + frameBytes);
    if (chunk.length < frameBytes) {
      const padded = Buffer.alloc(frameBytes);
      chunk.copy(padded, 0);
      chunk = padded;
    }
    socket.send(encodeFrame(seq, chunk));
    seq += 1;
    await new Promise((r) => setTimeout(r, FRAME_MS));
  }
  console.log(
    `\npumped ${seq} frames (~${seq * FRAME_MS} ms of audio) — sending stop`,
  );
  socket.send(JSON.stringify({ type: "stop", payload: { reason: "user" } }));

  // stop resolves after the server emits its single terminal `final`;
  // the server then closes 1000.
  for (;;) {
    const raw = await nextMessage(60_000, "final");
    if (raw === null) break; // socket closed
    const ev = JSON.parse(raw);
    const p = ev?.payload ?? {};
    switch (ev?.type) {
      case "partial":
        process.stdout.write(`\r› ${p.committed ?? ""}${p.tentative ?? ""}`);
        break;
      case "final":
        finals.push(p);
        console.log(`\n✓ final (#${finals.length} of the 1 expected)`);
        console.log(`  text: ${p.text}`);
        console.log(
          `  duration_ms=${p.duration_ms} language=${p.language} model_id=${p.model_id}`,
        );
        console.log(`  history_id=${p.history_id ?? "null"}`);
        break;
      case "speech_start":
        process.stdout.write("\n[speech_start] ");
        break;
      case "speech_end":
        process.stdout.write("\n[speech_end] ");
        break;
      case "error":
        console.error(
          `\n✗ server event ${p.code}: ${p.message} (retryable=${p.retryable})`,
        );
        break;
      default:
        break; // audio_ack / pong / unknown: observed and ignored
    }
  }
} catch (e) {
  console.error(`\nsession failed: ${e.message}`);
  process.exit(1);
}

socket.close(1000);
if (finals.length !== 1) {
  console.error(`✗ expected exactly one final, observed ${finals.length}`);
  process.exit(1);
}
console.log("✓ exactly one final received");
