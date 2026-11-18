#!/usr/bin/env bun
/**
 * 03-models.ts — the model catalog, selection, and managed downloads.
 *
 * Demonstrates:
 *   client.listModels({ installed: true }) — GET /v1/models?installed=true (safe filter;
 *                                            actual wire key is `installed`, not filter[…])
 *   client.getModel(id)                    — GET /v1/models/{id} → {model, prepare.stage}
 *   client.selectModel(id, "eager" | "lazy") — PUT /v1/models/selected (409 model_not_ready
 *                                              when the artifact is not local)
 *   client.downloadModel(id)               — POST /v1/models/{id}/download — STARTS or
 *                                            converges on a managed download (202/200)
 *   client.getDownload(id)                 — GET /v1/models/{id}/downloads (read-only poll)
 *   client.cancelModelDownload(id)         — DELETE /v1/models/{id}/download
 *
 * SAFETY: nothing here downloads a multi-GB artifact unless you pass --download.
 * Without flags it only lists and inspects. Model ids may contain slashes
 * (e.g. ggml/whisper); the SDK percent-encodes them and the server decodes once.
 *
 * Run:
 *   export VOXIX_TOKEN=vx_…
 *   bun examples/sdk/03-models.ts                       # list + inspect only
 *   bun examples/sdk/03-models.ts --model <id>          # inspect a specific model
 *   bun examples/sdk/03-models.ts --select <id> [eager|lazy]   # select (+load) — may 409
 *   bun examples/sdk/03-models.ts --download <id>       # STARTS a managed download (!)
 *   bun examples/sdk/03-models.ts --cancel-download <id>
 *
 * Expected output: a table of installed models, one model projection, selection echo
 * or managed-download status — depending on the flags given.
 * Exit codes: 0 ok · 1 failure · 2 usage (missing token / bad flags)
 */
import { VoxixClient } from "../../packages/sdk/src/client.js";
import { VoxixError } from "../../packages/sdk/src/errors.js";

const baseUrl = process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error(
    "usage: VOXIX_TOKEN=vx_… bun examples/sdk/03-models.ts [--model ID] [--select ID [eager|lazy]] [--download ID] [--cancel-download ID]",
  );
  process.exit(2);
}

const argv = process.argv.slice(2);
function flagValue(name: string): string | undefined {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
}
const modelFlag = flagValue("--model");
const selectFlag = flagValue("--select");
const selectIdx = argv.indexOf("--select");
const selectLoad: "eager" | "lazy" =
  selectFlag && selectIdx >= 0 && argv[selectIdx + 2] === "lazy"
    ? "lazy"
    : "eager";
const downloadFlag = flagValue("--download");
const cancelFlag = flagValue("--cancel-download");

const client = new VoxixClient({ baseUrl, token });

function explain(error: unknown): never {
  if (error instanceof VoxixError) {
    console.error(
      `${error.name}: code=${error.code} status=${error.status ?? "-"} remote=${error.remoteCode ?? "-"}\n  ${error.message}`,
    );
  } else {
    console.error((error as Error).message);
  }
  process.exit(1);
}

try {
  console.log("--- listModels({ installed: true }) ---");
  const page = await client.listModels({ installed: true });
  console.log(
    `selected=${page.selected_model_id ?? "<none>"} loaded=${page.loaded_model_id ?? "<none>"} installed=${page.models.length}`,
  );
  for (const m of page.models) {
    console.log(
      `  ${m.id} · ${m.name} · ${m.size_mb} MB · streaming=${m.supports_streaming} · download=${m.download.state}`,
    );
  }

  const inspectId = modelFlag ?? page.models[0]?.id;
  if (inspectId) {
    console.log(`--- getModel(${inspectId}) ---`);
    const single = await client.getModel(inspectId);
    console.log(JSON.stringify(single, null, 2));

    console.log(`--- getDownload(${inspectId}) ---`);
    const dl = await client.getDownload(inspectId);
    console.log(
      `state=${dl.state} bytes=${dl.downloaded_bytes}/${dl.total_bytes}${dl.percentage != null ? ` (${dl.percentage}%)` : ""}`,
    );
  } else {
    console.log("(no installed models; skipping getModel/getDownload)");
  }

  if (selectFlag) {
    // MUTATES daemon state (eager load can take seconds; 409 during inference leases).
    console.log(`--- selectModel(${selectFlag}, ${selectLoad}) ---`);
    const selection = await client.selectModel(selectFlag, selectLoad);
    console.log(
      `selected=${selection.model_id} load=${selection.load} loaded=${selection.loaded}`,
    );
  }

  if (downloadFlag) {
    // STARTS or converges on a managed download (multi-GB; partial files kept for resume).
    console.log(`--- downloadModel(${downloadFlag}) ---`);
    const status = await client.downloadModel(downloadFlag);
    console.log(
      `state=${status.state} bytes=${status.downloaded_bytes}/${status.total_bytes}`,
    );
    console.log(
      "(poll progress via: getDownload() / GET /v1/models/{id}/downloads)",
    );
  }

  if (cancelFlag) {
    console.log(`--- cancelModelDownload(${cancelFlag}) ---`);
    await client.cancelModelDownload(cancelFlag);
    console.log("cancel requested (partial files are kept for resume)");
  }

  if (!selectFlag && !downloadFlag && !cancelFlag) {
    console.log("\n(no mutating flags given — catalog inspection only)");
  }
} catch (error) {
  explain(error);
}
