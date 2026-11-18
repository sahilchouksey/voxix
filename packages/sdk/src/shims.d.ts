/**
 * Core-program ambient shims: the platform-neutral program compiles against
 * lib ES2022 only. These are the platform globals the core touches, typed
 * minimally and structurally — real runtime shapes (DOM lib / Node) remain
 * compatible; consumers running under `types: node` or DOM see their own
 * declarations instead. This file is consumed by the core tsconfig only and
 * is never emitted.
 */
type TimerHandle = object;

declare function setTimeout(handler: () => void, timeout?: number): TimerHandle;
declare function clearTimeout(handle: TimerHandle | undefined): void;
declare function setInterval(
  handler: () => void,
  timeout?: number,
): TimerHandle;
declare function clearInterval(handle: TimerHandle | undefined): void;

/** Minimal structural URL (transport factory signatures only). */
declare class URL {
  constructor(url: string | URL, base?: string | URL);
  toString(): string;
  readonly href: string;
}

/** Minimal structural Blob (batch upload seam). */
declare class Blob {
  constructor(parts?: ReadonlyArray<unknown>, options?: { type?: string });
  readonly type: string;
  readonly size: number;
  arrayBuffer(): Promise<ArrayBuffer>;
  text(): Promise<string>;
}

/** Minimal structural URLSearchParams (history paging helpers). */
declare class URLSearchParams {
  constructor(init?: string); // | Record<string, string>);
  get(name: string): string | null;
  set(name: string, value: string): void;
  toString(): string;
}

/** Minimal structural TextDecoder (SSE / ready-line byte decode). */
declare class TextDecoder {
  constructor(label?: string);
  decode(input?: Uint8Array): string;
}

interface Uint8ArrayConstructor {
  from(parts: readonly Uint8Array[]): Uint8Array;
}
