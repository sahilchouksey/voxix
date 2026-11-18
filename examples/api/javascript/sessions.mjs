#!/usr/bin/env node
// sessions.mjs — the session lifecycle over raw HTTP (create → get → complete → delete).
//
// Demonstrates:
//   POST   /v1/sessions        — admission-controlled create. Body is
//                                {"model_id"|"model"?, "language"?} plus the M2 keys
//                                (task/audio/vad/partial_results/save_history/
//                                client_session_key); unknown fields → 422.
//                                503 service_full (admission) / 503 service_busy
//                                (single-inference lease) / 503 engine_error otherwise.
//                                → 201 {session_id, session_token, state:"created",
//                                        ws_url, negotiated_audio, streaming_mode, …}
//   GET    /v1/sessions/{id}   — projection; needs bearer + X-Session-Token.
//                                Wrong/missing sub-token or unknown id → 404
//                                session_not_found (never 403 — ids cannot be probed)
//   PATCH  /v1/sessions/{id}   — {"state":"completed"} → 200 projection (lease released)
//                                {"state":"cancelled"} → 204 (lease released)
//                                unknown state → 422
//   DELETE /v1/sessions/{id}   — idempotent cancel → 204; terminal record retained
//                                for the retention window, then drops out of the registry
//
// Wire notes:
//   * Session routes require BOTH the bearer AND the per-session X-Session-Token
//     returned once in the 201 body. Treat it like a password.
//   * The WS media plane (audio in, partials/finals out) is NOT usable with plain
//     fetch — it needs a WebSocket client speaking the protocol-v1 frame discipline
//     . See sessions_stream.mjs for the working path.
//
// Runtime: node ≥ 20 (global fetch) or bun. No dependencies.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/sessions.mjs
//
// Expected output: the 422 envelope from the unknown-field probe (a no-op —
// nothing is allocated), then the 201 envelope, the GET projection, the PATCH
// result, and the two idempotent DELETEs.
// Exit codes: 0 ok · 1 request failure · 2 usage
const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(/\/+$/, "");
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}

const call = async (method, path, body, sessionToken) => {
  const headers = { Authorization: `Bearer ${token}` };
  if (sessionToken) headers["X-Session-Token"] = sessionToken;
  const init = { method, headers };
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
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
  // --- POST /v1/sessions with an UNKNOWN field — 422, nothing is allocated ---
  console.log('--- POST /v1/sessions {"model_idd": "typo"} (unknown field → 422) ---');
  let [status, body] = await call("POST", "/v1/sessions", { model_idd: "typo" });
  show("POST /v1/sessions (unknown field)", status, body);
  if (status !== 422) {
    console.error(`note: expected 422 for unknown field, got ${status} — is this an M3+ build?`);
  }

  // --- POST /v1/sessions — the real create ----------------------------------
  console.log("--- POST /v1/sessions ---");
  [status, body] = await call("POST", "/v1/sessions", {});
  if (status !== 201) {
    show("POST /v1/sessions", status, body);
    process.exit(1);
  }
  const created = JSON.parse(body);
  show("POST /v1/sessions", status, body);
  const sessionId = created.session_id;
  const sessionToken = created.session_token; // shown once; the projection never carries it

  // --- GET /v1/sessions/{id} --------------------------------------------------
  [status, body] = await call("GET", `/v1/sessions/${sessionId}`, undefined, sessionToken);
  show("GET /v1/sessions/{id} (echo before PATCH)", status, body);
  if (status !== 200) process.exit(1);

  // --- PATCH /v1/sessions/{id} {"state":"completed"} → 200 projection ---------
  [status, body] = await call("PATCH", `/v1/sessions/${sessionId}`, { state: "completed" }, sessionToken);
  show('PATCH /v1/sessions/{id} {"state":"completed"}', status, body);
  if (status !== 200) process.exit(1);

  // --- DELETE /v1/sessions/{id} — idempotent cancel, twice -------------------
  for (const attempt of [1, 2]) {
    [status, body] = await call("DELETE", `/v1/sessions/${sessionId}`, undefined, sessionToken);
    console.log(`--- DELETE /v1/sessions/{id} (attempt ${attempt}) → HTTP ${status} (204 expected, idempotent) ---`);
    if (status !== 204) {
      console.error(body);
      process.exit(1);
    }
  }
  console.log("done — session completed and delete is idempotent");
} catch (e) {
  console.error(`error: cannot reach ${base} (${e.cause?.message ?? e.message})`);
  process.exit(1);
}
