/**
 * Transport seams (spec §8 / §15 G19): the core never imports DOM or Node
 * platform types. `fetch` and `webSocketFactory` are structurally typed here
 * so user-supplied platform implementations (Node, Bun, browsers, RN fetch
 * polyfills, the `ws` package, OkHttp-backed bridges) slot in without a
 * dependency.
 *
 * Defaults resolve lazily against `globalThis` — `fetch` is assumed present
 * on Node ≥20/Bun/browsers; a global `WebSocket` is used when available
 * (Bun supports custom headers; browsers and Node ≥22's undici WebSocket do
 * NOT — when custom headers are required in such a runtime, supply
 * `webSocketFactory`).
 */
import { TransportError } from "./errors.js";

/** Minimal structural response. Compatible with global `fetch`'s Response. */
export interface FetchResponseLike {
  readonly ok: boolean;
  readonly status: number;
  readonly headers: { get(name: string): string | null };
  text(): Promise<string>;
  arrayBuffer(): Promise<ArrayBuffer>;
  json(): Promise<unknown>;
  /** Present on Node/Bun/browsers; typed structurally (ReadableStream-ish). */
  body?: {
    getReader(): {
      read(): Promise<{ done: boolean; value?: Uint8Array }>;
      releaseLock(): void;
    };
    cancel(): Promise<void>;
  };
}

/** Minimal structural fetch. Compatible with global `fetch`. */
export type FetchLike = (
  url: string,
  init?: {
    method?: string;
    headers?: Record<string, string>;
    body?: Uint8Array | string | undefined;
    signal?: AbortSignalLike;
  },
) => Promise<FetchResponseLike>;

/** Minimal structural AbortSignal. */
export interface AbortSignalLike {
  readonly aborted: boolean;
  addEventListener(type: "abort", listener: () => void): void;
  removeEventListener(type: "abort", listener: () => void): void;
}

export function isAbortSignal(value: unknown): value is AbortSignalLike {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as AbortSignalLike).aborted === "boolean" &&
    typeof (value as AbortSignalLike).addEventListener === "function"
  );
}

/** Whether the given (possibly user-injected) value looks like an AbortSignal. */
export function throwIfAborted(signal: AbortSignalLike | undefined): void {
  if (signal?.aborted) throw new CancelledAbort();
}

/** Internal sentinel for aborted request setup paths (mapped per call site). */
export class CancelledAbort extends Error {
  constructor() {
    super("operation aborted before it could start");
    this.name = "CancelledAbort";
  }
}

/* ------------------------------------------------------------------ */
/* WebSocket                                                           */
/* ------------------------------------------------------------------ */

/**
 * Minimal structural WebSocket. `readyState`/`bufferedAmount` follow the
 * platform convention (0 CONNECTING, 1 OPEN, 2 CLOSING, 3 CLOSED).
 */
export interface WebSocketLike {
  readonly readyState: number;
  /** Optional: sockets that expose it get real pushAudio backpressure. */
  readonly bufferedAmount?: number;
  send(data: string | Uint8Array): void;
  close(code?: number, reason?: string): void;
  onopen: (() => void) | null;
  onclose: ((ev: { code: number; reason: string }) => void) | null;
  onerror: ((ev: { message?: string }) => void) | null;
  /** Messages the socket delivers: JSON strings (events) may arrive alongside
   * binary frames — the service sends only text events; binaries are ignored. */
  onmessage: ((ev: { data: string | ArrayBuffer | Uint8Array }) => void) | null;
}

/**
 * Opens the media-plane socket. `init.headers` carries bearer/session
 * credentials; on runtimes whose WebSocket cannot send custom headers
 * (browsers, Node undici), the caller supplies a header-capable factory
 * (e.g. the `ws` package) or uses the ticket fallback path.
 */
export type WebSocketFactory = (
  url: string,
  init?: { headers?: Record<string, string> },
) => WebSocketLike;

/* `globalThis` access with structural casts (no DOM/Node lib dependency). */

interface GlobalWithFetch {
  fetch?: FetchLike;
}

/**
 * Minimal structural WebSocket constructor as found on globalThis. Accepts
 * the (url, options) shape (Bun and `ws`) and the (url, protocols) shape
 * (browsers, Node undici).
 */
interface WebSocketCtorLike {
  new (
    url: string | URL,
    options?: string[] | { headers?: Record<string, string>; protocols?: string | string[] },
  ): WebSocketLike;
}

interface GlobalWithWebSocket {
  WebSocket?: WebSocketCtorLike;
}

/** The default fetch: `globalThis.fetch` when present. */
export function defaultFetch(): FetchLike {
  const f = (globalThis as GlobalWithFetch).fetch;
  if (typeof f !== "function") {
    throw new TransportError({
      message:
        "no global fetch is available; supply the `fetch` option (this SDK does not load polyfills)",
      retryable: false,
    });
  }
  return f.bind(globalThis);
}

/* ------------------------------------------------------------------ */
/* UTF-8 decoding without DOM/Node globals                             */
/* ------------------------------------------------------------------ */

interface TextDecoderStructural {
  decode(input?: Uint8Array): string;
}

const GLOBAL_TEXT_DECODER = (globalThis as { TextDecoder?: new (constructorArg?: string) => TextDecoderStructural })
  .TextDecoder;

/** Decode UTF-8 where a TextDecoder exists; latin1 fallback otherwise
 * (ASCII content — SSE/JSON frames without multi-byte characters — is
 * byte-identical under the two). */
export function decodeUtf8(bytes: Uint8Array): string {
  if (GLOBAL_TEXT_DECODER) {
    return new GLOBAL_TEXT_DECODER().decode(bytes);
  }
  let out = "";
  for (const byte of bytes) out += String.fromCharCode(byte);
  return out;
}

/**
 * The default WebSocket adapter: `globalThis.WebSocket` when present.
 * Headers are passed through the (url, options) shape; a constructor that
 * rejects the options object throws immediately so callers fall back to an
 * injected factory instead of corrupting the handshake silently.
 */
export function defaultWebSocketFactory(): WebSocketFactory {
  const Ctor = (globalThis as GlobalWithWebSocket).WebSocket;
  if (typeof Ctor !== "function") {
    throw new TransportError({
      message:
        "no global WebSocket is available; supply the `webSocketFactory` option " +
        "(Node's undici WebSocket and browsers cannot attach custom headers — " +
        "the media plane needs them, use a header-capable adapter)",
      retryable: false,
    });
  }
  return (url, init) => {
    try {
      return new Ctor(url, { headers: init?.headers });
    } catch (cause) {
      // Constructor rejected the options shape (e.g. strict (url, protocols)
      // implementations) — retry without headers only when none were
      // required; otherwise surface the injection error honestly.
      if (init?.headers && Object.keys(init.headers).length > 0) {
        throw new TransportError({
          message:
            "the global WebSocket rejected custom headers; supply a `webSocketFactory` " +
            "that supports them (see the @voxix/sdk README), or authenticate this " +
            "socket via the ticket fallback (`ticketFallback: true`)",
          retryable: false,
          cause,
        });
      }
      return new Ctor(url);
    }
  };
}
