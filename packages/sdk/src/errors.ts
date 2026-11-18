/**
 * The SDK error model: exactly six concrete, tagged classes — every failure
 * maps onto one of them (spec §15 G4: no per-failure class taxonomy).
 *
 * Every error carries:
 * - `code`       — the SDK-side tag (`transport` | `auth` | `protocol` | `service` | `model` | `cancelled`)
 * - `status`     — the HTTP status when one is available
 * - `remoteCode` — the server-side `error.code` verbatim, when the service sent one
 * - `retryable`  — whether retrying may plausibly succeed
 * - `cause`      — the underlying thrown value, when one exists
 */

export type VoxixErrorCode =
  | "transport"
  | "auth"
  | "protocol"
  | "service"
  | "model"
  | "cancelled";

export const VOXIX_ERROR_CODE_NODE = "VoxixError";

export interface VoxixErrorInit {
  message: string;
  code: VoxixErrorCode;
  status?: number;
  remoteCode?: string;
  retryable?: boolean;
  cause?: unknown;
}

/** Base class for every error this SDK throws. Use `instanceof VoxixError`
 * to branch; narrow by `code` for the tag. */
export class VoxixError extends Error {
  readonly code: VoxixErrorCode;
  readonly status?: number;
  readonly remoteCode?: string;
  readonly retryable: boolean;

  constructor(init: VoxixErrorInit) {
    super(init.message, init.cause !== undefined ? { cause: init.cause } : undefined);
    this.name = `${VOXIX_ERROR_CODE_NODE}-${init.code}`;
    this.code = init.code;
    this.status = init.status;
    this.remoteCode = init.remoteCode;
    this.retryable = init.retryable ?? false;
  }
}

/** Network/socket-level failures: fetch threw, the WS died unexpectedly. */
export class TransportError extends VoxixError {
  constructor(init: Omit<VoxixErrorInit, "code">) {
    super({ ...init, code: "transport" });
  }
}

/** Authentication failures: 401 responses, rotated bearer tokens (close 4001). */
export class AuthError extends VoxixError {
  constructor(init: Omit<VoxixErrorInit, "code">) {
    super({ ...init, code: "auth" });
  }
}

/** Protocol discipline failures: malformed frames, bad `v`, bad close codes,
 * second-consumer tap registration, misuse of the SDK surface. */
export class ProtocolError extends VoxixError {
  constructor(init: Omit<VoxixErrorInit, "code">) {
    super({ ...init, code: "protocol" });
  }
}

/** Service-side failures surfaced via the standard error envelope or the WS
 * close codes: `service_busy`, `session_not_found`, encodings of 5xx, etc. */
export class ServiceError extends VoxixError {
  constructor(init: Omit<VoxixErrorInit, "code">) {
    super({ ...init, code: "service" });
  }
}

/** Model-plane failures: `model_not_found`, `model_not_ready`, `prepare-busy`. */
export class ModelError extends VoxixError {
  constructor(init: Omit<VoxixErrorInit, "code">) {
    super({ ...init, code: "model" });
  }
}

/** The user cancelled the work (AbortSignal, `cancel()`, `close()`). */
export class CancelledError extends VoxixError {
  constructor(init: Omit<VoxixErrorInit, "code" | "retryable">) {
    super({ ...init, code: "cancelled", retryable: false });
  }
}

/** Map an arbitrary thrown value from a fetch/socket call onto the tagged
 * classes (shared seam used by the client and session). */
export function wrapTransportFailure(cause: unknown): VoxixError {
  if (cause instanceof VoxixError) return cause;
  const isAbort = cause instanceof Error && cause.name === "AbortError";
  if (isAbort) {
    return new CancelledError({ message: "aborted", cause });
  }
  return new TransportError({
    message: cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "network request failed",
    retryable: true,
    cause,
  });
}
