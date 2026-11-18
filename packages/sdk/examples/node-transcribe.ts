#!/usr/bin/env bun
/**
 * Locally verifiable CLI example: connect to a running voxix daemon, stream
 * 16 kHz mono PCM16 frames from a WAV file, print partials and the final.
 *
 *   bunx voxix serve --bind 127.0.0.1:47832   # start your daemon (token on stdout/stderr)
 *   bun examples/node-transcribe.ts ./sample.wav [baseUrl] [token]
 *
 * Base URL / token resolution: argv > env (VOXIX_BASE_URL, VOXIX_TOKEN).
 * No auxiliary dependencies: the WAV reader comes from the SDK's node
 * subpath helpers (`@voxix/sdk/node`), framing is core (`pushAudio` builds
 * the §6 binary frames from Int16Array chunks).
 */
import { readFile } from "node:fs/promises";
import { VoxixClient } from "../src/client.js";
import { VoxixError } from "../src/errors.js";
import { wavToMonoPcm16 } from "../src/node/file.js";
import { samplesFor, type FrameDurationMs } from "../src/protocol/audio.js";

const [, , wavPathArg, baseUrlArg, tokenArg] = process.argv;
if (!wavPathArg) {
  console.error("usage: node-transcribe.ts <file.wav> [baseUrl] [token]");
  process.exit(2);
}
const baseUrl =
  baseUrlArg ?? process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
const token =
  tokenArg ??
  process.env.VOXIX_TOKEN ??
  envOrExit("token via argv[3] or VOXIX_TOKEN");

await transcribe(wavPathArg);

async function transcribe(wavPath: string): Promise<void> {
  process.on("SIGINT", () => {
    console.log("\nclosing");
    process.exit(0);
  });

  const client = new VoxixClient({ baseUrl, token });
  try {
    const health = await client.health();
    console.log(
      `daemon ${health.engine_version} · protocol v${health.protocol_version}`,
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
      `input is ${sampleRateHz} Hz — the v1 profile is 16 kHz mono; resample first (see examples/web-capture.ts)`,
    );
    process.exit(1);
  }

  const session = await client.createSession({
    vad: { mode: "server" },
    partial_results: true,
  });
  await session.connect();
  await session.start();
  console.log(
    `session ${session.id} · negotiated ${JSON.stringify(session.negotiatedAudio)}`,
  );

  const render = (async () => {
    for await (const event of session.events) {
      if (event.type === "partial") {
        process.stdout.write(
          `\r› ${event.payload.committed}${event.payload.tentative}`,
        );
      }
      if (event.type === "final") {
        console.log(`\n✓ final: ${event.payload.text}`);
        const historyId = event.payload.history_id;
        if (typeof historyId === "number")
          console.log(`  history #${historyId}`);
      }
      if (event.type === "error") {
        console.error(
          `\n✗ remote event ${event.payload.code}: ${event.payload.message}`,
        );
      }
    }
  })();

  try {
    // 20 ms chunks: 320 samples per push, zero-padded tail (§6 payload rule).
    const chunkMs: FrameDurationMs = 20;
    const chunkSamples = samplesFor(chunkMs);
    for (let offset = 0; offset < samples.length; offset += chunkSamples) {
      const slice = samples.subarray(
        offset,
        Math.min(offset + chunkSamples, samples.length),
      );
      const frame = new Int16Array(chunkSamples);
      frame.set(slice);
      await session.pushAudio(frame, chunkMs);
    }
    const final = await session.stop("end_of_stream");
    console.log(
      `streamed ${session.lastAck.through_seq + 1} frames (${final.duration_ms} ms audio)`,
    );
  } catch (error) {
    if (error instanceof VoxixError) {
      console.error(
        `\nsession failed — code=${error.code} remote=${error.remoteCode ?? "-"} status=${error.status ?? "-"} retryable=${error.retryable}\n  ${error.message}`,
      );
    } else {
      console.error(`\nsession failed: ${(error as Error).message}`);
    }
  } finally {
    await render;
    session.close();
  }
}

function envOrExit(name: string): string | undefined {
  const value = process.env[name];
  if (!value) {
    console.error(`missing token/baseUrl — pass argv or set ${name}`);
    process.exit(2);
  }
  return value;
}
