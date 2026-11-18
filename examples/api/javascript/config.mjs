#!/usr/bin/env node
// config.mjs — inspect and (non-destructively) patch the daemon config.
//
// Demonstrates:
//   GET   /v1/config   — the read projection (no secrets)
//   PATCH /v1/config   — the mutable set on this build:
//                        {allowed_origins, max_sessions (1–64), max_audio_seconds (1–7200),
//                         session_resume_window_ms (0–120000), pause_max_ms (0 or ≤3600000),
//                         save_history_default, vad_enabled_default, opus_enabled}
//                        Unknown fields answer 422; `bind` is not patchable (422).
//                        `opus_enabled:true` without the service-opus build answers 422.
//
// This example performs a NO-OP PATCH: it reads the current `max_audio_seconds`
// and writes the same value back, so daemon behavior is unchanged. Replace the
// value (keep the key inside the mutable set) to change it for real.
//
// Runtime: node ≥ 20 (global fetch) or bun. No dependencies.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/config.mjs
//
// Expected output: the config JSON before and after the patch (identical).
// Exit codes: 0 ok · 1 request failure (incl. 422 on daemons predating M3 PATCH) · 2 usage
// Note: protocol-v1.md §4 lists GET /v1/config as a "read-only projection (M1)";
// the PATCH route landed with M3 and is live on this build — wire reality wins.
const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(/\/+$/, "");
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}

const call = async (method, path, body) => {
  const init = { method, headers: { Authorization: `Bearer ${token}` } };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  const res = await fetch(base + path, init);
  return [res.status, await res.text()];
};

const show = (label, status, body) => {
  console.log(`--- ${label} → HTTP ${status} ---`);
  try {
    console.log(JSON.stringify(JSON.parse(body), null, 2));
  } catch {
    console.log(body || "(empty body)");
  }
};

try {
  let [status, body] = await call("GET", "/v1/config");
  console.log(`--- GET /v1/config → HTTP ${status} ---`);
  let config;
  try {
    config = JSON.parse(body);
  } catch {
    console.error(`GET failed: HTTP ${status} ${body}`);
    process.exit(1);
  }
  console.log(JSON.stringify(config, null, 2));
  if (status !== 200) process.exit(1);

  // Extract current max_audio_seconds (throws if the shape ever changes — fail loudly).
  const current = config.max_audio_seconds;
  console.log(`--- PATCH /v1/config {"max_audio_seconds": ${current}}  (same-value no-op) ---`);

  // Note: PATCH is NOT idempotent for read-only builds of the daemon; if your
  // build still rejects PATCH it answers 422 invalid_request and this step fails —
  // that is the signal the daemon predates M3 config.
  [status, body] = await call("PATCH", "/v1/config", { max_audio_seconds: current });
  show("PATCH /v1/config", status, body);
  process.exit(status === 200 ? 0 : 1);
} catch (e) {
  console.error(`error: cannot reach ${base} (${e.cause?.message ?? e.message})`);
  process.exit(1);
}
