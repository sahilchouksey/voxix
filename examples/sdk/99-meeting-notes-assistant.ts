#!/usr/bin/env bun
/**
 * 99-meeting-notes-assistant.ts — END-TO-END CAPSTONE: turn a folder of
 * interview WAV files into a `meeting-notes.md`.
 *
 * The real problem: after a week of 1:1s you have 12 .wav files and no notes.
 * This script preflights the daemon, transcribes every WAV through the batch
 * API, saves results incrementally (a crash never loses finished files), and
 * writes a markdown report: per-file transcripts, a merged "key points"
 * heuristic, and a snapshot of the saved history rows.
 *
 * Pipeline (each step is a section below):
 *   (a) preflight — status(): daemon mode, model state, active model, capabilities;
 *       --select MODEL_ID pins a model (selectModel, eager load — opt-in via flag)
 *   (b) catalog    — listModels({installed:true}): the daemon never implicitly
 *       downloads; refuse to start when nothing is installed
 *   (c) batch loop — for each *.wav (sorted): transcribeFile → poll getTranscription
 *       → collect. Failures are logged and processing CONTINUES with the next
 *       file; exit code is 1 at the end if any file failed
 *   (d) history    — history({limit: 5}): show the rows the daemon persisted
 *   (e) report     — write <out> with per-file transcripts + heuristic key points,
 *       then print next steps
 *
 * Key-points honesty: this is NO NLP model. It merges transcripts, splits on
 * sentence punctuation, and keeps the longest distinct sentences as bullets.
 * It is a starting scaffold for a human editor, not a summary.
 *
 * The report is rewritten after EVERY file (writeFile is atomic enough for a
 * single-user local tool), so partial results survive an interruption.
 *
 * SDK APIs + node stdlib only (fs/path/process) — no other dependencies.
 *
 * Run:
 *   export VOXIX_TOKEN=vx_…
 *   bun examples/sdk/99-meeting-notes-assistant.ts ./interviews/
 *   bun examples/sdk/99-meeting-notes-assistant.ts ./interviews/ --select ggml/whisper-large-v3-turbo
 *   bun examples/sdk/99-meeting-notes-assistant.ts ./interviews/ --out ./notes.md --timeout-ms 300000
 *
 * Expected output: preflight block, one log line per file, history snapshot,
 *   "meeting-notes.md written", next-steps list.
 * Exit codes: 0 every file transcribed · 1 any file failed or preflight failed · 2 usage
 */
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join, basename, extname, resolve } from "node:path";
import { VoxixClient } from "../../packages/sdk/src/client.js";
import { ServiceError, VoxixError } from "../../packages/sdk/src/errors.js";

const argv = process.argv.slice(2);
const folderArg = argv.find((a) => !a.startsWith("--"));
const selectArg = argv[argv.indexOf("--select") + 1];
const selectModel = selectArg && !selectArg.startsWith("--") ? selectArg : undefined;
const outArg = argv[argv.indexOf("--out") + 1];
const outPath = outArg && !outArg.startsWith("--") ? resolve(outArg) : undefined;
const timeoutArg = argv[argv.indexOf("--timeout-ms") + 1];
const timeoutMs = timeoutArg && !timeoutArg.startsWith("--") ? Number(timeoutArg) : 300_000;

const token = process.env.VOXIX_TOKEN;
const baseUrl = process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
if (!folderArg || Number.isNaN(timeoutMs)) {
  console.error("usage: VOXIX_TOKEN=vx_… bun examples/sdk/99-meeting-notes-assistant.ts <folder> [--select MODEL_ID] [--out FILE] [--timeout-ms N]");
  process.exit(2);
}
if (!token) {
  console.error("error: VOXIX_TOKEN is not set");
  process.exit(2);
}

const client = new VoxixClient({ baseUrl, token });
const folder = resolve(folderArg);
const reportPath = outPath ?? join(folder, "meeting-notes.md");

const fail = (msg: string): never => {
  console.error(`✗ ${msg}`);
  process.exit(1);
};

/** One row of the report: either a transcript or a per-file failure. */
interface FileResult {
  file: string;
  transcript?: string;
  durationMs?: number;
  error?: string;
}
const results: FileResult[] = [];

/** Heuristic key points: longest distinct sentences, capped at `limit`. */
function keyPoints(texts: string[], limit = 8): string[] {
  const merged = texts.join(" ");
  const sentences = merged
    .split(/(?<=[.!?])\s+/)
    .map((s) => s.trim())
    .filter((s) => s.length >= 40); // skip fragments; this is not NLP
  const seen = new Set<string>();
  const picked: string[] = [];
  for (const s of [...sentences].sort((a, b) => b.length - a.length)) {
    const key = s.toLowerCase().slice(0, 80);
    if (seen.has(key)) continue;
    seen.add(key);
    picked.push(s);
    if (picked.length >= limit) break;
  }
  return picked;
}

/** Rewrite the report from whatever we have so far — crash-safe by construction. */
async function writeReport(): Promise<void> {
  const done = results.filter((r) => r.transcript !== undefined);
  const failed = results.filter((r) => r.error !== undefined);
  const lines: string[] = [
    `# Meeting notes — ${basename(folder)}`,
    "",
    `Generated ${new Date().toISOString()} · ${done.length} transcribed / ${failed.length} failed · service ${baseUrl}`,
    "",
    "## Key points (heuristic — longest distinct sentences; edit freely)",
    "",
    ...keyPoints(done.map((r) => r.transcript ?? "")).map((s) => `- ${s}`),
    "",
    "## Transcripts",
    "",
  ];
  for (const r of results) {
    lines.push(`### ${r.file}`, "");
    lines.push(r.transcript ?? `**FAILED:** ${r.error}`, "");
  }
  if (failed.length > 0) {
    lines.push("## Failed files", "", ...failed.map((r) => `- ${r.file}: ${r.error}`), "");
  }
  await writeFile(reportPath, lines.join("\n"), "utf8");
}

try {
  // --- (a) preflight -------------------------------------------------------
  const status = await client.status();
  console.log("--- preflight: status() ---");
  console.log(`mode=${status.mode} · model_state=${status.model_state} · active=${status.active_model_id ?? "<none>"} · backend=${status.backend ?? "<?>"}`);
  console.log(`lease_held=${status.lease_held} · active_sessions=${status.active_sessions} · max_streams=${status.capabilities.max_concurrent_streams}`);

  if (selectModel) {
    // Opt-in mutation: pin the active model (eager load). 409 model_not_ready
    // when the artifact is not installed locally — never an implicit download.
    console.log(`--- selectModel(${selectModel}, eager) ---`);
    const sel = await client.selectModel(selectModel, "eager");
    console.log(`selected=${sel.model_id} loaded=${sel.loaded}`);
  }

  // --- (b) catalog: something must be installed locally ---------------------
  console.log("--- catalog: listModels({ installed: true }) ---");
  const page = await client.listModels({ installed: true });
  if (page.models.length === 0) {
    fail("no installed models — the daemon never downloads implicitly; install one in the app first");
  }
  for (const m of page.models) console.log(`  ✓ ${m.id} (${m.size_mb} MB, streaming=${m.supports_streaming})`);
  const chosen = status.active_model_id ?? page.models[0].id;
  console.log(`using model: ${chosen}`);

  // --- (c) batch loop -------------------------------------------------------
  const entries = await readdir(folder, { withFileTypes: true });
  const wavs = entries
    .filter((e) => e.isFile() && extname(e.name).toLowerCase() === ".wav")
    .map((e) => e.name)
    .sort();
  if (wavs.length === 0) fail(`no .wav files found in ${folder}`);
  console.log(`--- transcribing ${wavs.length} file(s) ---`);

  for (const name of wavs) {
    const filePath = join(folder, name);
    try {
      const buf = await readFile(filePath);
      const arrayBuffer = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
      const job = await client.transcribeFile(arrayBuffer);
      console.log(`▸ ${name} → job ${job.job_id}`);
      const deadline = Date.now() + timeoutMs;
      for (;;) {
        const st = await client.getTranscription(job.job_id);
        if (st.state === "completed") {
          results.push({ file: name, transcript: st.result?.transcript ?? "", durationMs: st.result?.duration_ms });
          console.log(`  ✓ completed (${st.result?.duration_ms ?? "?"} ms)`);
          break;
        }
        if (st.state === "failed" || st.state === "cancelled") {
          throw new Error(`job ${st.state}: ${st.error?.code ?? "?"} ${st.error?.message ?? ""}`);
        }
        if (Date.now() > deadline) throw new Error(`timed out after ${timeoutMs} ms (state=${st.state})`);
        await new Promise((r) => setTimeout(r, 500));
      }
    } catch (error) {
      const message =
        error instanceof ServiceError
          ? `status=${error.status} remote=${error.remoteCode ?? "-"} ${error.message}`
          : error instanceof VoxixError
            ? `${error.code}: ${error.message}`
            : (error as Error).message;
      console.error(`  ✗ ${name}: ${message}`);
      results.push({ file: name, error: message });
    }
    await writeReport(); // incremental — partial results are never lost
  }

  // --- (d) history snapshot --------------------------------------------------
  console.log("--- history: last 5 entries ---");
  try {
    const hist = await client.history({ limit: 5 });
    for (const e of hist.entries) {
      console.log(`  #${e.id} · ${new Date(e.timestamp_ms).toISOString()} · ${e.source} · ${e.duration_ms ?? "-"} ms`);
    }
    if (hist.entries.length === 0) console.log("  (history is empty — nothing persisted yet)");
  } catch (error) {
    // History is best-effort here: do not fail the run over the DB.
    const msg = error instanceof VoxixError ? error.message : (error as Error).message;
    console.log(`  (history unavailable: ${msg})`);
  }

  // --- (e) final report -------------------------------------------------------
  await writeReport();
  const failed = results.filter((r) => r.error !== undefined);
  console.log(`\n✓ meeting-notes.md written → ${reportPath}`);
  console.log("next steps:");
  console.log("  1. edit the key points — heuristic bullets are scaffolding, not prose");
  console.log("  2. failed files (if any) can be re-run with the same command; completed jobs are history rows");
  console.log("  3. grep the report for names and attach it to your 1:1 tracker");
  if (failed.length > 0) {
    console.error(`\n✗ ${failed.length} of ${results.length} file(s) failed — see above; partial results are saved`);
    process.exit(1);
  }
  process.exit(0);
} catch (error) {
  if (error instanceof VoxixError) {
    fail(`${error.name}: code=${error.code} status=${error.status ?? "-"} remote=${error.remoteCode ?? "-"} — ${error.message}`);
  }
  fail((error as Error).message);
}
