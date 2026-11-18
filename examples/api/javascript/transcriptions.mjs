#!/usr/bin/env node
// transcriptions.mjs — batch (non-streaming) transcription over raw HTTP from JavaScript.
//
// SDK twin: ../../sdk/04-batch.ts — same flow with the typed SDK client and the
// six-class error taxonomy. If you are already writing TypeScript, use that.
//
// Demonstrates:
//   POST   /v1/transcriptions        — raw WAV body (application/octet-stream), 64 MiB cap;
//                                      202 {job_id, state:"processing", status_url}
//                                      Rejections: 429 queue-full · 409 service_busy
//                                      (live session holds the inference lease) · 400 invalid-audio
//   GET    /v1/transcriptions/{job}  — poll; states: processing → completed|failed|cancelled
//                                      404 not_found for unknown ids
//   DELETE /v1/transcriptions/{job}  — best-effort cancel: 204 while processing,
//                                      409 job_settled once terminal, 404 unknown
//
// Wire fidelity notes:
//   * The body is the WAV bytes verbatim (node's Buffer — a Uint8Array) — no multipart, no JSON, no FormData.
//   * Error bodies always carry {"error":{"code","message","retryable","request_id","details?"}}.
//
// Runtime: node ≥ 20 (global fetch) or bun. No dependencies.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/transcriptions.mjs ./speech.wav
//   node examples/api/javascript/transcriptions.mjs ./speech.wav --cancel-after-start
//
// Expected output: the 202 envelope, poll lines, then the transcript (or a clear failure).
// Exit codes: 0 completed · 1 failed/cancelled/timed out · 2 usage
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";

const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(
  /\/+$/,
  "",
);
const token = process.env.VOXIX_TOKEN;
const [wavPath, ...flagArgs] = process.argv.slice(2);
const flags = flagArgs;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}
if (
  !wavPath ||
  !existsSync(wavPath) ||
  flags.some((f) => f !== "--cancel-after-start")
) {
  console.error("usage: transcriptions.mjs <file.wav> [--cancel-after-start]");
  process.exit(2);
}

const auth = { Authorization: `Bearer ${token}` };
const show = (label, status, body) => {
  console.log(`--- ${label} → HTTP ${status} ---`);
  try {
    console.log(JSON.stringify(JSON.parse(body), null, 2));
  } catch {
    console.log(body || "(empty body)");
  }
};

try {
  // --- POST /v1/transcriptions -------------------------------------------
  const wav = await readFile(wavPath); // Buffer (a Uint8Array) — the WAV bytes, verbatim
  console.log(`--- POST /v1/transcriptions (${wav.byteLength} bytes) ---`);
  const post = await fetch(`${base}/v1/transcriptions`, {
    method: "POST",
    headers: { ...auth, "Content-Type": "application/octet-stream" },
    body: wav,
  });
  if (!post.ok) {
    // Every non-2xx /v1 response carries {error:{code,message,retryable,…}}
    show("POST /v1/transcriptions", post.status, await post.text());
    process.exit(1);
  }
  const accepted = await post.json(); // {job_id, state:"processing", status_url}
  console.log(JSON.stringify(accepted, null, 2));

  if (flags.includes("--cancel-after-start")) {
    // --- DELETE /v1/transcriptions/{job} — best-effort cancel (204 vs 409) ---
    const del = await fetch(`${base}/v1/transcriptions/${accepted.job_id}`, {
      method: "DELETE",
      headers: auth,
    });
    console.log(
      `DELETE → HTTP ${del.status} (204 = cancelled while processing, 409 = already settled, 404 = unknown)`,
    );
    process.exit(del.status === 204 || del.status === 409 ? 0 : 1);
  }

  // --- GET /v1/transcriptions/{job} — poll loop ---------------------------
  console.log(`--- GET /v1/transcriptions/${accepted.job_id} (polling) ---`);
  const deadline = Date.now() + 300_000;
  for (;;) {
    const res = await fetch(`${base}/v1/transcriptions/${accepted.job_id}`, {
      headers: auth,
    });
    if (!res.ok) {
      show("GET /v1/transcriptions/{job}", res.status, await res.text());
      process.exit(1);
    }
    const s = await res.json();
    if (s.state === "completed") {
      show("job completed", res.status, JSON.stringify(s));
      console.log("--- transcript ---");
      console.log(s.result?.transcript ?? "");
      process.exit(0);
    }
    if (s.state === "failed" || s.state === "cancelled") {
      show("job ended", res.status, JSON.stringify(s));
      console.error(
        `job ${s.state}: ${s.error?.code ?? "?"} ${s.error?.message ?? ""}`,
      );
      process.exit(1);
    }
    if (Date.now() > deadline) {
      console.error(
        `timed out after 300 s (job is still processing; retry or DELETE ${accepted.job_id})`,
      );
      process.exit(1);
    }
    console.log(`  … ${s.state}`);
    await new Promise((r) => setTimeout(r, 250));
  }
} catch (e) {
  console.error(
    `error: cannot reach ${base} (${e.cause?.message ?? e.message})`,
  );
  process.exit(1);
}
