#!/usr/bin/env node
// history.mjs — paginated transcript history.
//
// Demonstrates:
//   GET /v1/history?limit=N&cursor=M — entries newest-first; page shape
//                                      {entries:[…], has_more, next_cursor?}.
//                                      `limit` clamps to 1–100 (default 20);
//                                      pass `next_cursor` back as `cursor` to
//                                      walk older pages.
//   GET /v1/history/{id}             — one entry by integer id (404 not_found otherwise)
//
// Entries are written by the daemon when a batch job completes or a stream
// finalizes with save_history on (see save_history_default in /v1/config).
// The route may answer 503 history_unavailable on builds without the history
// database — that is a capability signal, not a token problem.
//
// PRIVACY: this prints your private transcripts to the terminal. Only run it on
// a machine you trust, and don't pipe the output somewhere you wouldn't paste
// your own dictations.
//
// Runtime: node ≥ 20 (global fetch) or bun. No dependencies.
//
// Run:
//   export VOXIX_TOKEN=vx_…
//   node examples/api/javascript/history.mjs [LIMIT]
//
// Expected output: a compact table of the newest entries (id, time, source, title).
// Exit codes: 0 ok (including an empty history) · 1 request/503/404 failure · 2 usage
const base = (process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832").replace(
  /\/+$/,
  "",
);
const token = process.env.VOXIX_TOKEN;
const limit = process.argv[2] ?? "5";
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}
if (!/^\d+$/.test(limit)) {
  console.error(
    "usage: history.mjs [LIMIT]  (LIMIT must be an integer; clamps to 1–100)",
  );
  process.exit(2);
}

try {
  console.log(`--- GET /v1/history?limit=${limit} ---`);
  const res = await fetch(`${base}/v1/history?limit=${limit}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  const body = await res.text();
  let page;
  try {
    page = JSON.parse(body);
  } catch {
    console.error(`GET failed: HTTP ${res.status} ${body}`);
    process.exit(1);
  }
  if (!res.ok) {
    console.error(JSON.stringify(page, null, 2));
    process.exit(1);
  }
  console.log(JSON.stringify(page, null, 2));

  const entries = page.entries ?? [];
  if (entries.length === 0) {
    console.log(
      "(empty — nothing recorded yet; run a batch job or a stream with save_history on)",
    );
  } else {
    console.log();
    console.log(
      `${"ID".padStart(6)}  ${"WHEN".padEnd(19)}  ${"SRC".padEnd(6)}  TITLE`,
    );
    for (const e of entries) {
      const when = new Date(e.timestamp_ms ?? 0).toLocaleString("sv-SE"); // YYYY-MM-DD HH:MM:SS
      console.log(
        `${String(e.id ?? "?").padStart(6)}  ${when.padEnd(19)}  ${String(e.source ?? "?").padEnd(6)}  ${String(e.title ?? "").slice(0, 60)}`,
      );
    }
    if (page.has_more) {
      console.log(
        `(more entries — pass cursor=${page.next_cursor} to walk older pages)`,
      );
    }
  }
} catch (e) {
  console.error(
    `error: cannot reach ${base} (${e.cause?.message ?? e.message})`,
  );
  process.exit(1);
}
