#!/usr/bin/env bun
/**
 * 04-batch.ts — batch transcription with the SDK: submit → poll → print.
 *
 * Demonstrates:
 *   client.transcribeFile(arrayBuffer)  — POST /v1/transcriptions (raw WAV bytes) →
 *                                         BatchJob {job_id, state, status_url}
 *   client.getTranscription(job_id)     — GET /v1/transcriptions/{job_id} → poll until
 *                                         state is completed|failed|cancelled
 *   client.cancelTranscription(job_id)  — DELETE /v1/transcriptions/{job_id}
 *                                         (behind --cancel; resolves false on 404/409)
 *
 * SDK twin of ../api/transcriptions.{sh,mjs,py}. Error handling shows the full
 * error taxonomy: VoxixError (base) → code transport|auth|protocol|service|model|cancelled;
 * ServiceError adds .status and .remoteCode (the server's error.code verbatim).
 *
 * The SDK is not on npm — imports resolve the in-repo source by relative path.
 *
 * Run:
 *   export VOXIX_TOKEN=vx_…
 *   bun examples/sdk/04-batch.ts ./speech.wav
 *   bun examples/sdk/04-batch.ts ./speech.wav --cancel   # demonstrate the cancel path
 *   bun examples/sdk/04-batch.ts ./speech.wav --timeout-ms 120000
 *
 * Expected output: job id, poll dots, then the transcript (or a tagged error).
 * Exit codes: 0 completed · 1 failed/cancelled/timeout · 2 usage (missing token)
 */
// Ported from the original examples/transcribe.ts (kept lineage); upgraded to the
// full envelope taxonomy + cancel demo + timeout flag.
import { readFile } from "node:fs/promises";
import { VoxixClient } from "../../packages/sdk/src/client.js";
import { ServiceError, VoxixError } from "../../packages/sdk/src/errors.js";

const argv = process.argv.slice(2);
const wavPath = argv.find((a) => !a.startsWith("--"));
const doCancel = argv.includes("--cancel");
const timeoutFlag = argv[argv.indexOf("--timeout-ms") + 1];
const timeoutMs =
  timeoutFlag && !timeoutFlag.startsWith("--") ? Number(timeoutFlag) : 120_000;

if (!wavPath || Number.isNaN(timeoutMs)) {
  console.error(
    "usage: VOXIX_TOKEN=vx_… bun examples/sdk/04-batch.ts ./speech.wav [--cancel] [--timeout-ms N]",
  );
  process.exit(2);
}
const baseUrl = process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}

const client = new VoxixClient({ baseUrl, token });

const buf = await readFile(wavPath);
const arrayBuffer = buf.buffer.slice(
  buf.byteOffset,
  buf.byteOffset + buf.byteLength,
);

try {
  const job = await client.transcribeFile(arrayBuffer);
  console.log(`submitted job ${job.job_id} (state=${job.state})`);

  if (doCancel) {
    // Best-effort cancel: true = cancelled, false = already settled/unknown.
    const cancelled = await client.cancelTranscription(job.job_id);
    console.log(
      cancelled
        ? "cancel requested (204)"
        : "job already terminal — nothing to cancel (404/409)",
    );
    // Fall through and poll once so the outcome is visible.
  }

  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const status = await client.getTranscription(job.job_id);
    if (status.state === "completed") {
      console.log(status.result?.transcript ?? "");
      process.exit(0);
    }
    if (status.state === "failed") {
      console.error(
        `transcription failed: ${status.error?.code ?? "?"} ${status.error?.message ?? ""}`,
      );
      process.exit(1);
    }
    if (status.state === "cancelled") {
      console.error("transcription was cancelled");
      process.exit(1);
    }
    if (Date.now() > deadline) {
      console.error(
        `timed out after ${timeoutMs} ms (job still ${status.state})`,
      );
      process.exit(1);
    }
    process.stdout.write(".");
    await new Promise((r) => setTimeout(r, 250));
  }
} catch (error) {
  if (error instanceof ServiceError) {
    // 4xx/5xx with a service envelope, e.g. 429 queue-full, 409 service_busy, 400 invalid-audio.
    console.error(
      `ServiceError: status=${error.status} remote=${error.remoteCode ?? "-"} retryable=${error.retryable}\n  ${error.message}`,
    );
  } else if (error instanceof VoxixError) {
    console.error(
      `${error.name}: code=${error.code} status=${error.status ?? "-"} remote=${error.remoteCode ?? "-"}\n  ${error.message}`,
    );
  } else {
    console.error((error as Error).message);
  }
  process.exit(1);
}
