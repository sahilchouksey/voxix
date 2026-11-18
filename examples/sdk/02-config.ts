#!/usr/bin/env bun
/**
 * 02-config.ts — read and (non-destructively) patch the daemon config.
 *
 * Demonstrates:
 *   client.config()                  — GET /v1/config — the no-secrets projection
 *   client.patchConfig({…})          — PATCH /v1/config — the M3 mutable set:
 *                                      {allowed_origins, max_sessions (1–64),
 *                                       max_audio_seconds (1–7200),
 *                                       session_resume_window_ms (0–120000),
 *                                       pause_max_ms (0 or ≤3600000),
 *                                       save_history_default, vad_enabled_default,
 *                                       opus_enabled (needs the service-opus build)}
 *                                      Unknown fields → 422; `bind` is not patchable.
 *
 * The patch below rewrites the CURRENT max_audio_seconds value — a true no-op
 * that still exercises validation, the auth path and the response envelope.
 * Change the value to something within 1–7200 to mutate the daemon for real.
 *
 * Run:
 *   export VOXIX_TOKEN=vx_…
 *   bun examples/sdk/02-config.ts
 *
 * Expected output: the config before and after the (identical) patch.
 * Exit codes: 0 ok · 1 failure (422/auth/transport) · 2 usage (missing token)
 */
import { VoxixClient } from "../../packages/sdk/src/client.js";
import { VoxixError } from "../../packages/sdk/src/errors.js";

const baseUrl = process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("usage: VOXIX_TOKEN=vx_… bun examples/sdk/02-config.ts");
  process.exit(2);
}

const client = new VoxixClient({ baseUrl, token });

function printConfig(label: string, cfg: Awaited<ReturnType<typeof client.config>>): void {
  console.log(`--- ${label} ---`);
  console.log(JSON.stringify(cfg, null, 2));
}

try {
  const before = await client.config();
  printConfig("GET /v1/config (before)", before);

  // Same-value patch: validates the wire path without changing daemon state.
  const after = await client.patchConfig({ max_audio_seconds: before.max_audio_seconds });
  printConfig(`PATCH /v1/config {max_audio_seconds: ${before.max_audio_seconds}} (no-op)`, after);

  if (after.max_audio_seconds === before.max_audio_seconds) {
    console.log("\npatch round-tripped; daemon config unchanged");
  } else {
    console.error("\nwarning: config changed despite same-value patch");
    process.exit(1);
  }
} catch (error) {
  if (error instanceof VoxixError) {
    console.error(`${error.name}: code=${error.code} status=${error.status ?? "-"} remote=${error.remoteCode ?? "-"}\n  ${error.message}`);
  } else {
    console.error((error as Error).message);
  }
  process.exit(1);
}
