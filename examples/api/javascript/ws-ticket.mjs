#!/usr/bin/env node
// ws-ticket.mjs — mint a one-use ticket for the WebSocket upgrade.
//
// Demonstrates:
//   POST /v1/auth/ws-ticket  {"session_id"?} → 201
//     {ticket, expires_at} — a 60-second, one-use credential for upgrading
//     GET /v1/sessions/{id}/stream with `?ticket=…` instead of the bearer
//     header (runtimes where Authorization cannot be attached to a socket).
//
// Semantics ( §11 + bearer middleware):
//   * Bearer remains required for REST; the ticket substitutes for the bearer
//     ONLY on the WS upgrade path — and only when it is valid, unexpired and
//     unconsumed. Consumption happens inside the upgrade handler.
//   * With session_id set, the ticket is bound to that session; consuming it
//     against any other session id fails.
//   * X-Session-Token is STILL required on the upgrade — the ticket only
//     replaces the bearer.
//   * A second socket per session is refused 503 service_full.
//
// The ticket is single-use and bearer-equivalent for 60 s — it is printed only
// with --print-token. Otherwise only a redacted prefix is shown.
//
// Runtime: node ≥ 20 (global fetch) or bun. No dependencies.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/ws-ticket.mjs                # unbound ticket (redacted)
//   node examples/api/javascript/ws-ticket.mjs <session-id>   # bound to that session
//   node examples/api/javascript/ws-ticket.mjs --print-token  # actually print the ticket
//
// Expected output: the 201 status and redacted ticket metadata (or the ticket itself).
// Exit codes: 0 ok · 1 request failure · 2 usage
const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(/\/+$/, "");
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}

const argv = process.argv.slice(2);
const printToken = argv.includes("--print-token");
const rest = argv.filter((a) => a !== "--print-token");
if (rest.length > 1) {
  console.error("usage: ws-ticket.mjs [SESSION_ID] [--print-token]");
  process.exit(2);
}
const sessionId = rest[0];

try {
  const res = await fetch(`${base}/v1/auth/ws-ticket`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify(sessionId ? { session_id: sessionId } : {}),
  });
  const body = await res.text();
  console.log(`--- POST /v1/auth/ws-ticket → HTTP ${res.status} (201 expected) ---`);
  let doc;
  try {
    doc = JSON.parse(body);
  } catch {
    console.log(body || "(empty body)");
    process.exit(res.status === 201 ? 0 : 1);
  }
  if (res.status !== 201) {
    console.log(JSON.stringify(doc, null, 2));
    process.exit(1);
  }
  if (printToken) {
    console.log(JSON.stringify(doc, null, 2));
  } else {
    const redacted = { ...doc, ticket: `${String(doc.ticket ?? "").slice(0, 6)}… (redacted — pass --print-token to reveal)` };
    console.log(JSON.stringify(redacted, null, 2));
  }
  console.log("(note: the ticket is single-use and dies in 60 s — do not retry it)");
} catch (e) {
  console.error(`error: cannot reach ${base} (${e.cause?.message ?? e.message})`);
  process.exit(1);
}
