import { describe, expect, test } from "bun:test";
import { startHarness, type Harness } from "./mock-server.js";
import { create, parseReadyLine, type ChildProcessLike } from "../src/node/create.js";
import { ProtocolError, TransportError } from "../src/errors.js";

/** Assembles a ChildProcessLike that emits a scripted stdout/stderr flow. */
function fakeChild(lines: {
  stdout?: string;
  stderr?: string;
  pid?: number;
  exit?: number;
}): ChildProcessLike {
  const stdoutCallbacks: Array<(chunk: Uint8Array | string) => void> = [];
  const exitCallbacks: Array<(code: number | null, signal: string | null) => void> = [];
  const child: ChildProcessLike = {
    pid: lines.pid ?? 424242,
    stdin: {
      write: () => undefined,
      end: () => {
        (child.__ends as number[]).push(1);
      },
    },
    stdout: {
      on: (event: "data" | "end", cb: (c?: unknown) => void) => {
        if (event === "data") stdoutCallbacks.push(cb as never);
        process.nextTick(() => cb?.());
      },
    },
    stderr: {
      on: (event: "data" | "end", cb: (c?: unknown) => void) => {
        if (event === "data" && lines.stderr) cb(lines.stderr);
        process.nextTick(() => cb?.());
      },
    },
    kill: () => {
      (child.__kills as string[]).push("SIGTERM");
    },
    onExit: (cb) => {
      exitCallbacks.push(cb);
    },
  };
  (child as { __ends: number[] }).__ends = [];
  (child as { __kills: string[] }).__kills = [];
  queueMicrotask(() => {
    if (lines.stdout) for (const cb of stdoutCallbacks) cb(lines.stdout);
    if (lines.exit !== undefined) for (const cb of exitCallbacks) cb(lines.exit, null);
  });
  return child as ChildProcessLike & { __ends: number[]; __kills: string[] };
}

describe("embedded-child create() (§15 G1.1/G24)", () => {
  test("parseReadyLine accepts the dual-key contract; rejects violations", () => {
    const line = JSON.stringify({
      type: "ready",
      url: "http://127.0.0.1:45678",
      token: "vx_abc",
      apiVersion: "1",
      api_version: 1,
      pid: 777,
    });
    const parsed = parseReadyLine(line);
    expect(parsed.token).toBe("vx_abc");
    expect(parsed.pid).toBe(777);

    expect(() => parseReadyLine('{"type":"other"}')).toThrow(/"ready"/);
    expect(() =>
      parseReadyLine(JSON.stringify({ type: "ready", url: "http://10.0.0.1:47", token: "vx_x", pid: 1 })),
    ).toThrow(/loopback/);
    expect(() =>
      parseReadyLine(JSON.stringify({ type: "ready", url: "http://127.0.0.1:47", token: "plain", pid: 1 })),
    ).toThrow(/vx_/);
    expect(() =>
      parseReadyLine(JSON.stringify({ type: "ready", url: "http://127.0.0.1:47", token: "vx_x", pid: "seven" })),
    ).toThrow(/pid/);
    expect(() => parseReadyLine(JSON.stringify({ type: "ready", url: "http://127.0.0.1:47", token: "vx_x", pid: 1, api_version: 2 }))).toThrow(/api_version/);
    expect(() => parseReadyLine("x".repeat(5_000))).toThrow(/byte cap/);
  });

  test("create binds a client to the ready lease and waits for prepare", async () => {
    const harness: Harness = await startHarness({ transcript: "embed" });
    try {
      const child = fakeChild({
        stdout:
          JSON.stringify({
            type: "ready",
            url: harness.baseUrl,
            token: harness.token,
            apiVersion: "1",
            api_version: 1,
            pid: 424242,
          }) + "\n",
        pid: 424242,
      });
      const prepared: string[] = [];
      const client = await create({
        command: "fake-voxix",
        spawn: () => child,
        model: "tiny-16k",
        onPrepare: (event) => prepared.push(event.event),
      });
      expect(prepared).toEqual(["verifying", "loading", "ok"]);
      const health = await client.health();
      expect(health.ready).toBe(true);
      await client.dispose();
      expect((child as { __ends: number[] }).__ends.length).toBe(1);
    } finally {
      await harness.stop();
    }
  });

  test("pid cross-check failure and stderr retention on early exit", async () => {
    const mismatch = await create({
      command: "fake",
      spawn: () =>
        fakeChild({
          stdout: JSON.stringify({ type: "ready", url: "http://127.0.0.1:47", token: "vx_a", pid: 9999 }) + "\n",
          pid: 7,
        }),
    }).catch((e) => e);
    expect(mismatch).toBeInstanceOf(ProtocolError);
    expect((mismatch as Error).message).toContain("does not match the spawned child");

    const exited = await create({
      command: "fake",
      spawn: () => fakeChild({ stderr: "boom: bad flag", exit: 1 }),
    }).catch((e) => e);
    expect(exited).toBeInstanceOf(TransportError);
    expect((exited as Error).message).toContain("stderr head: boom");
  });

  test("non-JSON ready line rejects with ProtocolError", async () => {
    const garbage = await create({
      command: "fake",
      spawn: () => fakeChild({ stdout: "not-json-at-all\n", pid: 5 }),
    }).catch((e) => e);
    expect(garbage).toBeInstanceOf(ProtocolError);
  });
});
