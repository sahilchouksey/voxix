#!/usr/bin/env bun
/**
 * 07-embed-child.ts — OPTIONAL: spawn an embedded `voxix serve --embed` child
 * and talk to it over the loopback control plane.
 *
 * Demonstrates @voxix/sdk/node's create() (packages/sdk/src/node/create.ts):
 *   const child = await create({ command }) — spawns `voxix serve --embed`,
 *       binds an ephemeral 127.0.0.1 port, mints an in-memory bearer, and
 *       resolves with a ready-made VoxixClient once the child prints its single
 *       ready line ({"type":"ready", url, token, pid, apiVersion/api_version}) on
 *       stdout. The child's stdin is the lifetime lease: dispose() ends stdin,
 *       the child drains and exits 0. Nothing is persisted on either side.
 *
 * create() exists in this build. If `voxix` is not on PATH (the embedded
 * layout is a packaging concern — see packaging/), this example
 * prints SKIP and exits 0 rather than failing your tour.
 *
 * Run:
 *   export VOXIX_BASE_URL=…   # ignored here — the child picks its own port
 *   bun examples/sdk/07-embed-child.ts                 # command from PATH
 *   bun examples/sdk/07-embed-child.ts --command /path/to/voxix
 *
 * Expected output: launch line, health JSON of the embedded child, clean exit.
 * Exit codes: 0 ok or skipped · 1 child failed the handshake · 2 usage
 */
import { create } from "../../packages/sdk/src/node/index.js";
import { TransportError, ProtocolError, VoxixError } from "../../packages/sdk/src/errors.js";

const commandFlag = process.argv[process.argv.indexOf("--command") + 1];
const command = process.argv.includes("--command") && commandFlag && !commandFlag.startsWith("--")
  ? commandFlag
  : "voxix";

try {
  // create() validates the ready line (loopback URL, vx_ token, integer pid
  // cross-checked against the spawned child) and wires the client for you —
  // you never see the token.
  const child = await create({ command });
  console.log("embedded child ready — running health check");
  const health = await child.health();
  console.log(JSON.stringify(health, null, 2));
  await child.dispose(); // ends stdin → the child settles and exits 0
  console.log("disposed (stdin lease ended) — embedded child exiting");
  process.exit(0);
} catch (error) {
  if (error instanceof TransportError || error instanceof ProtocolError) {
    // The overwhelmingly common cause here is a missing `voxix` binary.
    console.log(`SKIP: embedded child did not come up (${error.message})`);
    console.log("this example needs the packaged `voxix` binary on PATH — see packaging/");
    process.exit(0);
  }
  if (error instanceof VoxixError) {
    console.error(`${error.name}: code=${error.code}\n  ${error.message}`);
  } else {
    console.error((error as Error).message);
  }
  process.exit(1);
}
