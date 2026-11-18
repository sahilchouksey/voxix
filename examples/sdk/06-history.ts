#!/usr/bin/env bun
/**
 * 06-history.ts — iterate the saved-transcript history with cursor pagination.
 *
 * Demonstrates:
 *   client.history({ limit, cursor? }) — GET /v1/history?limit=N[&cursor=M]
 *       page shape: { entries: HistoryEntry[], has_more, next_cursor? }
 *       limit clamps to 1–100 (default 20); pass next_cursor back as cursor
 *       to walk older pages. History is written by the daemon when a batch
 *       job completes (save_history_default) or a stream final persists one.
 *
 * Run:
 *   export VOXIX_TOKEN=vx_…
 *   bun examples/sdk/06-history.ts                 # newest 10, one page
 *   bun examples/sdk/06-history.ts 25 --all        # 25 per page, every page
 *
 * Expected output: a table of entries (id · date · source · duration · model ·
 * first line of text) plus a total count.
 * Exit codes: 0 ok (an empty history is not an error) · 1 failure (incl. 503
 *   history_unavailable on builds without the history DB) · 2 usage
 */
import { VoxixClient } from "../../packages/sdk/src/client.js";
import { ServiceError, VoxixError } from "../../packages/sdk/src/errors.js";

const baseUrl = process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error("usage: VOXIX_TOKEN=vx_… bun examples/sdk/06-history.ts [LIMIT] [--all]");
  process.exit(2);
}
const limitArg = process.argv.slice(2).find((a) => !a.startsWith("--"));
const limit = limitArg ? Number(limitArg) : 10;
const all = process.argv.includes("--all");
if (!Number.isInteger(limit) || limit < 1 || limit > 100) {
  console.error("LIMIT must be an integer in 1–100");
  process.exit(2);
}

const client = new VoxixClient({ baseUrl, token });

const fmtTime = (ms: number) => new Date(ms).toISOString().replace("T", " ").replace(/\..*$/, "Z");
const clip = (s: string, n: number) => (s.length <= n ? s : `${s.slice(0, n - 1)}…`);

let printed = 0;
let cursor: number | undefined;
try {
  for (;;) {
    const page = await client.history({ limit, ...(cursor !== undefined ? { cursor } : {}) });
    for (const e of page.entries) {
      printed += 1;
      const firstLine = clip(e.transcription_text.split("\n").find((l) => l.trim())?.trim() ?? "", 60);
      console.log(
        `#${e.id} · ${fmtTime(e.timestamp_ms)} · ${e.source} · ${e.duration_ms ?? "-"} ms · ${e.model_id ?? "-"} · ${e.language ?? "auto"}`,
      );
      console.log(`    ${firstLine}`);
    }
    if (!all || !page.has_more || page.next_cursor == null) break;
    cursor = page.next_cursor;
  }
  console.log(printed === 0 ? "history is empty (nothing transcribed with history on yet)" : `\n${printed} entr${printed === 1 ? "y" : "ies"}`);
} catch (error) {
  if (error instanceof ServiceError) {
    // 503 history_unavailable = capability signal (no history DB on this build).
    console.error(`ServiceError: status=${error.status} remote=${error.remoteCode ?? "-"}\n  ${error.message}`);
  } else if (error instanceof VoxixError) {
    console.error(`${error.name}: code=${error.code} status=${error.status ?? "-"}\n  ${error.message}`);
  } else {
    console.error((error as Error).message);
  }
  process.exit(1);
}
