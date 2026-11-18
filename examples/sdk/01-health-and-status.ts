#!/usr/bin/env bun
/**
 * 01-health-and-status.ts — alive(), health(), status() and the capability block.
 *
 * Demonstrates the SDK's three liveness/metadata probes:
 *   client.alive()  — GET /alive (no auth); false on any failure, never throws
 *   client.health() — GET /v1/health (bearer) — version/engine_version/protocol_version/uptime
 *   client.status() — GET /v1/status (bearer) — mode, bind, model state, lease, capabilities
 *
 * The SDK is not on npm — imports resolve the in-repo source by relative path.
 *
 * Run:
 *   export VOXIX_TOKEN=vx_…
 *   bun examples/sdk/01-health-and-status.ts
 *
 * Expected output: one line per probe plus a printed capability block, e.g.
 *   alive=true
 *   daemon 1.4.7+abc1234 · protocol v1 · uptime 81241 ms · ready=true
 *   ...
 * Exit codes: 0 ok · 1 unreachable/auth failure · 2 usage (missing token)
 */
import { VoxixClient } from "../../packages/sdk/src/client.js";
import { VoxixError } from "../../packages/sdk/src/errors.js";

const baseUrl = process.env.VOXIX_BASE_URL ?? "http://127.0.0.1:47832";
const token = process.env.VOXIX_TOKEN;
if (!token) {
  console.error(
    "usage: VOXIX_TOKEN=vx_… bun examples/sdk/01-health-and-status.ts",
  );
  process.exit(2);
}

const client = new VoxixClient({ baseUrl, token });

try {
  const alive = await client.alive();
  console.log(`alive=${alive}`);
  if (!alive) {
    console.error(
      `daemon did not answer /alive at ${baseUrl} — is it running?`,
    );
    process.exit(1);
  }

  const health = await client.health();
  console.log(
    `daemon ${health.engine_version} · protocol v${health.protocol_version} · uptime ${health.uptime_ms} ms · ready=${health.ready}`,
  );

  const status = await client.status();
  console.log(`mode=${status.mode} · bind=${status.bind}`);
  console.log(
    `model=${status.active_model_id ?? "<none>"} · state=${status.model_state} · backend=${status.backend ?? "<unknown>"}`,
  );
  console.log(
    `active_sessions=${status.active_sessions} · lease_held=${status.lease_held} · max_realtime_factor=${status.max_realtime_factor ?? "<n/a>"}`,
  );
  console.log("capabilities (explicit keys, never omitted):");
  for (const [key, value] of Object.entries(status.capabilities)) {
    console.log(`  ${key}=${value}`);
  }
} catch (error) {
  if (error instanceof VoxixError) {
    // Auth failures are 401 + empty body; everything else carries the service envelope.
    console.error(
      `${error.name}: code=${error.code} status=${error.status ?? "-"} remote=${error.remoteCode ?? "-"}\n  ${error.message}`,
    );
  } else {
    console.error((error as Error).message);
  }
  process.exit(1);
}
