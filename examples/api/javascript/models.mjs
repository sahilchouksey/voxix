#!/usr/bin/env node
// models.mjs — the model catalog surface.
//
// Demonstrates:
//   GET  /v1/models?installed=true   — list installed models only
//                                      (wire shape is `installed=true|false`, NOT filter[…];
//                                       omit the query param to list everything)
//   GET  /v1/models/{id}             — one model + prepare-stage projection
//                                      (ids may contain slashes, e.g. ggml/whisper — the
//                                       wildcard route decodes percent-escapes once)
//   GET  /v1/models/{id}/downloads   — managed-download status projection
//                                      {state,downloaded_bytes,total_bytes,percentage?}
//   PUT  /v1/models/selected         — select + eagerly load a model  [--select ID]
//                                      body: {"model_id": "…", "load": "eager"|"lazy"}
//                                      ("eager" default; PUT — not POST)
//                                      409 model_not_ready when not installed locally —
//                                      the service never implicitly downloads
//
// DEFAULT IS PRINT-ONLY: running this example never changes daemon state. The PUT
// selection step loads the model (it holds the inference lease), so it is gated
// behind `--select ID`. Do not pass it unless you mean to switch the active model.
//
// Runtime: node ≥ 20 (global fetch) or bun. No dependencies.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/models.mjs              # list + show first installed model
//   node examples/api/javascript/models.mjs ggml/whisper # show a specific model id
//   node examples/api/javascript/models.mjs --select ID  # ALSO PUT the selection (mutates!)
//
// Expected output: models list, one-model projection, download status.
// Exit codes: 0 ok · 1 request failure · 2 usage
const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(/\/+$/, "");
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}

const argv = process.argv.slice(2);
let select = null;
const selectIdx = argv.indexOf("--select");
if (selectIdx !== -1) {
  select = argv[selectIdx + 1];
  if (!select) {
    console.error("usage: models.mjs [MODEL_ID] [--select MODEL_ID]");
    process.exit(2);
  }
  argv.splice(selectIdx, 2);
}
const modelIdArg = argv[0] ?? null;

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
  console.log("--- GET /v1/models?installed=true ---");
  let [status, body] = await call("GET", "/v1/models?installed=true");
  let modelsDoc;
  try {
    modelsDoc = JSON.parse(body);
  } catch {
    console.error(`GET failed: HTTP ${status} ${body}`);
    process.exit(1);
  }
  console.log(JSON.stringify(modelsDoc, null, 2));
  if (status !== 200) process.exit(1);

  const models = modelsDoc.models ?? [];
  const modelId = modelIdArg ?? select ?? models[0]?.id ?? null;
  if (!modelId) {
    console.error("error: no installed models found and no MODEL_ID argument given\n" +
                  "       (the daemon never implicitly downloads; install a model in the app first)");
    process.exit(2);
  }
  console.log(`(using MODEL_ID=${modelId})`);

  // Model ids may contain `/` — fetch passes the path verbatim (the server decodes once).
  [status, body] = await call("GET", `/v1/models/${modelId}`);
  show(`GET /v1/models/${modelId}`, status, body);
  if (status !== 200) process.exit(1);
  [status, body] = await call("GET", `/v1/models/${modelId}/downloads`);
  show(`GET /v1/models/${modelId}/downloads`, status, body);

  if (select) {
    // PUT /v1/models/selected MUTATES daemon state (it eagerly loads the model on this
    // build, and 409s during an inference lease). Explicitly requested via --select.
    console.log(`--- PUT /v1/models/selected {"model_id": "${select}", "load": "eager"} ---`);
    [status, body] = await call("PUT", "/v1/models/selected", { model_id: select, load: "eager" });
    show("PUT /v1/models/selected", status, body);
    process.exit(status === 200 ? 0 : 1);
  } else {
    console.log("(skipping PUT /v1/models/selected — pass --select MODEL_ID to run it; it loads the model)");
  }
} catch (e) {
  console.error(`error: cannot reach ${base} (${e.cause?.message ?? e.message})`);
  process.exit(1);
}
