#!/usr/bin/env bun
/**
 * 05-streaming.ts — full WebSocket media-plane session: WAV file → server VAD → final.
 *
 * Demonstrates the streaming path the raw api/ tree cannot reach with curl:
 *   client.createSession({ vad: { mode: "server" }, partial_results: true })
 *       — POST /v1/sessions → VoxixSession (201 carries ws_url + session_token)
 *   session.connect()       — WS upgrade (bearer + X-Session-Token) → server hello
 *   session.start()         — flips the streaming phase; no audio before `started`
 *   session.pushAudio(frame, 20)
 *       — binary frames: 12-byte header ("VX" + v1 + codec + u32 seq + u16 dur
 *         + u16 flags) + i16le mono PCM; pushed at real-time ~20 ms cadence
 *   session.events          — bounded single-consumer tap: partial | final | error | …
 *   session.stop("user")    — FIFO finalize barrier; resolves the one terminal `final`
 *   negotiated_audio        — echoed from the 201 body (pcm_s16le / 16 kHz / 20 ms)
 *
 * SDK twin of packages/sdk/examples/node-transcribe.ts, carried into the root
 * examples with protocol-level commentary. Needs a running daemon with an
 * installed model (streaming capability depends on the model — see
 * supports_streaming in the catalog; final_only mode still yields exactly one final).
 *
 * The SDK is not on npm — imports resolve the in-repo source by relative path.
 *
 * Run:
 *   export VOXIX_TOKEN=vx_…
 *   bun examples/sdk/05-streaming.ts ./speech.wav
 *
 * Expected output: session banner, live partials on one line, then exactly one
 *   "final:" line with the transcript, duration, and history_id (null on this build
 *   unless save_history persisted one).
 * Exit codes: 0 final received · 1 transport/protocol/service failure · 2 usage
 */
import { readFile } from "node:fs/promises";
import { VoxixClient } from "../../packages/sdk/src/client.js";
import { VoxixError } from "../../packages/sdk/src/errors.js";
import { wavToMonoPcm16 } from "../../packages/sdk/src/node/file.js";
import {
  samplesFor,
  type FrameDurationMs,
} from "../../packages/sdk/src/protocol/audio.js";

const wavPath = process.argv[2];
const baseUrl = process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
const token = process.env.VOXIX_TOKEN;
if (!wavPath) {
  console.error(
    "usage: VOXIX_TOKEN=vx_… bun examples/sdk/05-streaming.ts ./speech.wav",
  );
  process.exit(2);
}
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}

const client = new VoxixClient({ baseUrl, token });

// Fail fast with a readable message when the daemon is down or the token is wrong.
try {
  const health = await client.health();
  console.log(
    `daemon ${health.engine_version} · protocol v${health.protocol_version} · ready=${health.ready}`,
  );
} catch (error) {
  console.error(
    `daemon unreachable at ${baseUrl}: ${(error as Error).message}`,
  );
  process.exit(1);
}

const bytes = await readFile(wavPath);
const { samples, sampleRateHz } = wavToMonoPcm16(bytes);
if (sampleRateHz !== 16_000) {
  console.error(
    `input is ${sampleRateHz} Hz — the v1 audio profile is 16 kHz mono s16le; resample first`,
  );
  process.exit(1);
}
console.log(
  `decoded ${samples.length} samples (${Math.round((samples.length / 16_000) * 1000)} ms)`,
);

// --- create -------------------------------------------------------------
// Server-side VAD auto-segments speech; client treats input as speech-only;
// disabled feeds everything through. partial_results surfaces `partial` events
// (only in native streaming mode; final_only models emit exactly one `final`).
const session = await client.createSession({
  vad: { mode: "server" },
  partial_results: true,
});
await session.connect(); // hello barrier: no start before hello
await session.start(); // started barrier: NO AUDIO BEFORE THIS LINE
console.log(
  `session ${session.id} · negotiated ${JSON.stringify(session.negotiatedAudio)}`,
);

// --- observe -------------------------------------------------------------
let finalCount = 0;
const render = (async () => {
  for await (const event of session.events) {
    if (event.type === "partial") {
      process.stdout.write(
        `\r› ${event.payload.committed}${event.payload.tentative}`,
      );
    } else if (event.type === "final") {
      finalCount += 1;
      // history_id: string | number | null — null on this build unless history is enabled.
      console.log(`\n✓ final (#${finalCount} of the 1 expected)`);
      console.log(`  text: ${event.payload.text}`);
      console.log(
        `  duration_ms=${event.payload.duration_ms} language=${event.payload.language} model_id=${event.payload.model_id}`,
      );
      console.log(`  history_id=${event.payload.history_id ?? "null"}`);
    } else if (event.type === "speech_start") {
      process.stdout.write("\n[speech_start] ");
    } else if (event.type === "speech_end") {
      process.stdout.write("\n[speech_end] ");
    } else if (event.type === "error") {
      console.error(
        `\n✗ server event ${event.payload.code}: ${event.payload.message} (retryable=${event.payload.retryable})`,
      );
    }
  }
})();

// --- pump -----------------------------------------------------------------
// 20 ms = 320 samples per frame (§11.4). Real-time pacing keeps server VAD
// honest; the zero-padded tail matches the declared FrameDurationMs.
const chunkMs: FrameDurationMs = 20;
const chunkSamples = samplesFor(chunkMs);
try {
  for (let offset = 0; offset < samples.length; offset += chunkSamples) {
    const slice = samples.subarray(
      offset,
      Math.min(offset + chunkSamples, samples.length),
    );
    const frame = new Int16Array(chunkSamples); // zero-padded tail
    frame.set(slice);
    await session.pushAudio(frame, chunkMs); // synchronous encode+send; seq from 0
    // Real-time cadence so server-side VAD observes natural timing.
    await new Promise((r) => setTimeout(r, chunkMs));
  }

  // FIFO barrier: every pre-stop frame is fed before the decode starts; stop
  // resolves after the server emits its single terminal `final` (close 1000 follows).
  const final = await session.stop("user");
  console.log(
    `stop resolved · streamed ~${Math.round((samples.length / 16_000) * 1000)} ms audio · server saw ${final.duration_ms} ms`,
  );
} catch (error) {
  if (error instanceof VoxixError) {
    console.error(
      `\nsession failed — code=${error.code} remote=${error.remoteCode ?? "-"} status=${error.status ?? "-"} retryable=${error.retryable}\n  ${error.message}`,
    );
  } else {
    console.error(`\nsession failed: ${(error as Error).message}`);
  }
  session.close(); // local teardown closes the tap — await render cannot hang
  await render; // drain any events that raced the failure
  process.exit(1);
}

// On success the server closes 1000 after the terminal final, which closes the tap.
await render;

if (finalCount !== 1) {
  console.error(`✗ expected exactly one final, observed ${finalCount}`);
  session.close();
  process.exit(1);
}
console.log(`✓ exactly one final received`);
session.close();
process.exit(0);
