/**
 * Embedded-child launcher and the model-bound ready constructor (spec §12 M4,
 * §15 G1.1/G24): spawn `voxix serve --embed`, validate exactly one ready
 * line, cross-check the pid against the spawned child, keep stdin open as
 * the lifetime lease, and optionally wait for the session model to finish
 * prepare before resolving.
 *
 * Only the injected `spawn` seam touches processes (no static `node:`
 * imports — the core/web programs stay free of Node typings; Node-shaped
 * runtimes resolve the default spawn lazily).
 */
import { ProtocolError, TransportError } from "../errors.js";
import { VoxixClient, VoxixClientOptions } from "../client.js";
import { PrepareEvent } from "../protocol/index.js";

/* -- structural process view ---------------------------------------- */

export interface ReadyLine {
  type: string;
  url: string;
  token: string;
  /** Dual keys: consumer-contract `apiVersion:"1"` and spec `api_version:1`. */
  apiVersion?: string;
  api_version?: number;
  pid: number;
}

export interface ReadableLike {
  on(event: "data", cb: (chunk: Uint8Array | string) => void): void;
  on(event: "end", cb: () => void): void;
}

export interface ChildProcessLike {
  pid?: number;
  stdin: {
    write(chunk: string, encoding?: string): void;
    end(): void;
  };
  stdout: ReadableLike;
  stderr: ReadableLike;
  kill(signal?: string): void;
  onExit(cb: (code: number | null, signal: string | null) => void): void;
}

export type SpawnLike = (
  command: string,
  args: string[],
  options: { stdio: ["pipe", "pipe", "pipe"] },
) => ChildProcessLike;

export interface CreateOptions {
  /** Explicit command; default resolves `voxix` on PATH. */
  command?: string | string[];
  /** Extra argv appended before `serve --embed` (used with string command). */
  argv?: string[];
  /** Model id bound at creation: create() resolves only after prepare. */
  model?: string;
  /** Per-prepare callback (G24 progress/prepare semantics). */
  onPrepare?: (event: PrepareEvent) => void;
  /** Spawn override (tests inject a fake child). */
  spawn?: SpawnLike;
  /** Extra client options (timeouts, transports, ticketFallback). */
  client?: Partial<VoxixClientOptions>;
  /** Coarse abort handle for the launcher itself (poll, plus explicitly
   * closing the child when set). */
  signal?: { aborted: boolean };
}

export interface EmbeddedClient extends VoxixClient {
  /** Ends stdin: the child settles in-flight calls and exits 0 (§6 lease).
   * Idempotent; a failed or cancelled creation closes it already. */
  dispose(): Promise<void>;
}

/* ------------------------------------------------------------------ */

const READY_MAX_BYTES = 4096;
const LOOPBACK_URL = /^http:\/\/(127\.0\.0\.1|\[::1\]|localhost)(:\d+)?\/?$/;

/**
 * Parse + validate a ready line: loopback http URL, `vx_`-prefixed token,
 * positive-integer pid, supported api version, bounded size. Additive
 * payload fields are ignored (wire-evolution rule).
 */
export function parseReadyLine(line: string): ReadyLine {
  const trimmed = line.trim();
  if (trimmed.length === 0) {
    throw new ProtocolError({ message: "ready line is empty" });
  }
  if (trimmed.length > READY_MAX_BYTES) {
    throw new ProtocolError({ message: "ready line exceeds the byte cap" });
  }
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(trimmed) as Record<string, unknown>;
  } catch {
    throw new ProtocolError({ message: "ready line is not JSON" });
  }
  if (typeof parsed !== "object" || parsed === null) {
    throw new ProtocolError({ message: "ready line is not an object" });
  }
  if (parsed.type !== "ready") {
    throw new ProtocolError({
      message: `ready line type is ${String(parsed.type)} ("ready" expected)`,
    });
  }
  const url = typeof parsed.url === "string" ? parsed.url : "";
  if (!LOOPBACK_URL.test(url)) {
    throw new ProtocolError({
      message: `ready url is not the loopback contract: ${url}`,
    });
  }
  const token = typeof parsed.token === "string" ? parsed.token : "";
  if (!token.startsWith("vx_")) {
    throw new ProtocolError({ message: "ready token lacks the vx_ prefix" });
  }
  if (
    typeof parsed.pid !== "number" ||
    !Number.isInteger(parsed.pid) ||
    parsed.pid <= 0
  ) {
    throw new ProtocolError({ message: "ready pid is not a positive integer" });
  }
  const apiVersion = parsed.apiVersion;
  const apiVersionInt = parsed.api_version;
  const supported =
    apiVersionInt === 1 ||
    apiVersion === "1" ||
    (apiVersionInt === undefined && apiVersion === undefined);
  if (!supported) {
    throw new ProtocolError({
      message: "ready line carries an unsupported api_version",
    });
  }
  return {
    type: "ready",
    url,
    token,
    apiVersion: typeof apiVersion === "string" ? apiVersion : undefined,
    api_version: typeof apiVersionInt === "number" ? apiVersionInt : undefined,
    pid: parsed.pid,
  };
}

/* ------------------------------------------------------------------ */

async function defaultSpawn(): Promise<SpawnLike> {
  const moduleName = "node:child_process";
  const mod = (await import(/* @vite-ignore */ moduleName).catch(
    () => undefined,
  )) as { spawn?: unknown } | undefined;
  if (typeof mod?.spawn !== "function") {
    throw new TransportError({
      message:
        "no spawn implementation available (supply the `spawn` option outside Node/Bun)",
      retryable: false,
    });
  }
  return mod.spawn as unknown as SpawnLike;
}

function decodeChunk(chunk: Uint8Array | string): string {
  return typeof chunk === "string" ? chunk : new TextDecoder().decode(chunk);
}

/** Bounded string accumulator with a data pump. */
function pump(stream: ReadableLike, sink: (chunk: string) => void): void {
  stream.on("data", (chunk) => sink(decodeChunk(chunk)));
}

/**
 * Spawn the embedded engine and return a VoxixClient bound to the
 * self-authenticating pairwise lease. `options.model` upgrades the call to
 * the model-bound ready constructor: it resolves only after prepare
 * completes; a failed or cancelled creation closes the child before
 * rejecting, while an already-aborted request never closes a healthy child
 * (§15 G24).
 */
export async function create(
  options: CreateOptions = {},
): Promise<EmbeddedClient> {
  const spawnFn = options.spawn ?? (await defaultSpawn());
  const [command, extra] = Array.isArray(options.command)
    ? [options.command[0], options.command.slice(1)]
    : [options.command ?? "voxix", options.argv ?? []];
  const args = [...extra, "serve", "--embed"];

  const child = spawnFn(command, args, { stdio: ["pipe", "pipe", "pipe"] });
  let ready: ReadyLine | undefined;
  let stdoutBuffer = "";
  let stderrHead = "";
  let lineSeen = false;
  const endLease = () => {
    try {
      child.stdin.end();
    } catch {
      /* stdin already closed */
    }
  };
  const closeChild = () => {
    try {
      child.kill("SIGTERM");
    } catch {
      /* child already gone */
    }
    endLease();
  };
  pump(child.stderr, (chunk) => {
    if (stderrHead.length < 2048) stderrHead += chunk;
  });
  if (options.signal?.aborted) {
    // An already-aborted request never closes a healthy child — it never
    // spawned work to abort beyond kill() before it started.
    closeChild();
    throw new ProtocolError({ message: "create() aborted before launch" });
  }

  const spawnDiagnostic = (message: string): TransportError =>
    new TransportError({
      message:
        stderrHead.length > 0
          ? `${message} — stderr head: ${stderrHead.slice(0, 512)}`
          : message,
      retryable: false,
    });

  try {
    ready = await new Promise<ReadyLine>((resolve, reject) => {
      let settled = false;
      const finish = (fn: () => void) => {
        if (settled) return;
        settled = true;
        fn();
      };
      child.onExit(() => {
        finish(() =>
          reject(
            spawnDiagnostic("the child exited before emitting the ready line"),
          ),
        );
      });
      pump(child.stdout, (chunk) => {
        if (lineSeen) return;
        stdoutBuffer += chunk;
        const nl = stdoutBuffer.indexOf("\n");
        if (nl < 0) {
          if (stdoutBuffer.length > READY_MAX_BYTES) {
            finish(() =>
              reject(spawnDiagnostic("ready line spilled past the byte cap")),
            );
          }
          return;
        }
        lineSeen = true;
        const line = stdoutBuffer.slice(0, nl);
        stdoutBuffer = "";
        try {
          const parsed = parseReadyLine(line);
          if (
            typeof child.pid === "number" &&
            child.pid > 0 &&
            parsed.pid !== child.pid
          ) {
            finish(() =>
              reject(
                new ProtocolError({
                  message: `ready pid ${parsed.pid} does not match the spawned child ${child.pid}`,
                }),
              ),
            );
            return;
          }
          finish(() => resolve(parsed));
        } catch (cause) {
          finish(() => reject(cause));
        }
      });
      if (options.signal?.aborted) {
        finish(() =>
          reject(new ProtocolError({ message: "create() aborted" })),
        );
      }
    });
  } catch (cause) {
    closeChild();
    throw cause;
  }

  const client = new VoxixClient({
    ...(options.client ?? {}),
    baseUrl: ready.url,
    token: ready.token,
  }) as EmbeddedClient;
  let disposed = false;
  (client as { dispose(): Promise<void> }).dispose = async () => {
    if (disposed) return;
    disposed = true;
    endLease();
  };

  if (options.model) {
    try {
      if (options.signal?.aborted)
        throw new ProtocolError({ message: "create() aborted" });
      await client.prepareModel(options.model, options.onPrepare);
    } catch (cause) {
      await client.dispose();
      closeChild();
      throw cause;
    }
  }

  return client;
}
