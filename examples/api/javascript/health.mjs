#!/usr/bin/env node
// health.mjs — liveness + bearer-authed health/status probes.
//
// Demonstrates:
//   GET /alive       — unauthenticated liveness (supervisors/proxies)
//   GET /v1/health   — bearer-authed: version, engine_version, protocol_version, uptime, ready
//   GET /v1/status   — bearer-authed: mode/bind/active model, model_state, lease, capabilities
//
// Runtime: node ≥ 20 (global fetch) or bun. No dependencies.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/health.mjs
//
// Expected output: three pretty-printed JSON documents.
//   { "alive": true }
//   { "status": "ok", "version": "…", "engine_version": "…+<sha>",
//     "protocol_version": 1, "uptime_ms": N, "ready": true }
//   { "mode": "…", "bind": "127.0.0.1:…", "active_model_id": …, "model_state": "…",
//     "capabilities": { "max_concurrent_streams": 1, … }, … }
// Exit codes: 0 ok · 1 request failure (401 = missing/wrong token) · 2 usage
// Note: /alive never needs a token; every other /v1/* route does (401 + empty body on failure).
const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(/\/+$/, "");
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set (expected the vx_… bearer printed at first daemon start)");
  process.exit(2);
}

const show = (label, status, body) => {
  console.log(`--- ${label} → HTTP ${status} ---`);
  try {
    console.log(JSON.stringify(JSON.parse(body), null, 2));
  } catch {
    console.log(body || "(empty body)");
  }
  if (status >= 400) process.exit(1);
};

const get = async (path, auth) => {
  const headers = auth ? { Authorization: `Bearer ${token}` } : {};
  const res = await fetch(base + path, { headers });
  return [res.status, await res.text()];
};

const run = async () => {
  let [status, body] = await get("/alive", false).catch((e) => {
    console.error(`error: cannot reach ${base} (${e.cause?.message ?? e.message})`);
    process.exit(1);
  });
  show("GET /alive (no auth)", status, body);
  [status, body] = await get("/v1/health", true);
  show("GET /v1/health (bearer)", status, body);
  [status, body] = await get("/v1/status", true);
  show("GET /v1/status (bearer)", status, body);
};

await run();
