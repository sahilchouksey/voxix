/**
 * VoxixClient — the REST control-plane surface over protocol v1, plus the
 * session factory. Injected `fetch` (spec §8/§15 G19); every method maps
 * non-2xx responses onto the six tagged error classes via the service
 * envelope (`{error:{code,message,retryable,...}}`).
 */
import {
  AuthError,
  CancelledError,
  ModelError,
  ProtocolError,
  ServiceError,
  TransportError,
  VoxixError,
} from "./errors.js";
import {
  BatchJob,
  BatchJobStatus,
  CreatedSession,
  CreateSessionBody,
  Health,
  HistoryPage,
  ModelDownload,
  ModelDownloadStatus,
  ModelSelection,
  ModelSelectionLoad,
  ModelsPage,
  PrepareEvent,
  ServiceConfig,
  ServiceConfigPatch,
  ServerStatus,
  SessionView,
  SingleModel,
  VxErrorEnvelope,
  WsTicket,
} from "./protocol/index.js";
import {
  decodeUtf8,
  defaultFetch,
  FetchLike,
  FetchResponseLike,
  AbortSignalLike,
} from "./transport.js";
import { defaultWebSocketFactory, WebSocketFactory } from "./transport.js";
import { wrapTransportFailure } from "./errors.js";
import { VoxixSession, type VoxixSessionOptions, type VoxixSessionConfig } from "./session.js";

/* ------------------------------------------------------------------ */
/* Options                                                             */
/* ------------------------------------------------------------------ */

export interface VoxixClientOptions {
  /** Service base, e.g. `http://127.0.0.1:47832` (trailing slash tolerated,
   * reverse-proxy path prefixes work). */
  baseUrl: string;
  /** Plaintext bearer token (`vx_…`) or a provider resolving it per call
   * (rotation-aware consumers). The `Bearer ` scheme is added for you. */
  token: string | (() => Promise<string> | string);
  /** Custom fetch (REST/batch/SSE). Defaults to `globalThis.fetch`. */
  fetch?: FetchLike;
  /** Custom WebSocket opener for the media plane. Defaults to
   * `globalThis.WebSocket` where custom headers are supported. */
  webSocketFactory?: WebSocketFactory;
  /**
   * Use a one-use `?ticket=` for the WS upgrade instead of the bearer
   * header (runtimes where Authorization cannot be attached to the socket).
   * The ticket substitutes the bearer only — `X-Session-Token` is still
   * required on the upgrade (server G9/G14).
   */
  ticketFallback?: boolean;
  /** Session handshake timeouts (ms). Defaults: hello 10s, started 30s,
   * final 60s, close grace 5s, app-level ping every 5s. */
  timeouts?: Partial<SessionTimeouts>;
}

export interface SessionTimeouts {
  helloMs: number;
  startedMs: number;
  finalMs: number;
  closeGraceMs: number;
  pingIntervalMs: number;
}

export const DEFAULT_TIMEOUTS: SessionTimeouts = {
  helloMs: 10_000,
  startedMs: 30_000,
  finalMs: 60_000,
  closeGraceMs: 5_000,
  pingIntervalMs: 5_000,
};

/* ------------------------------------------------------------------ */
/* Helpers                                                             */
/* ------------------------------------------------------------------ */

function joinUrl(base: string, path: string): string {
  return `${base.replace(/\/+$/, "")}${path.startsWith("/") ? path : `/${path}`}`;
}

function renderToken(token: string | (() => Promise<string> | string)): Promise<string> {
  return typeof token === "function" ? Promise.resolve(token()) : Promise.resolve(token);
}

/** Terminal model-plane codes that map to ModelError. */
const MODEL_ERROR_CODES = new Set(["model_not_found", "model_not_ready", "prepare-busy"]);

function joinBytes(parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((n, p) => n + p.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.byteLength;
  }
  return out;
}

function stripCr(bytes: Uint8Array): Uint8Array {
  if (bytes.length === 0) return bytes;
  if (bytes[bytes.length - 1] !== 0x0d) return bytes;
  return bytes.subarray(0, bytes.length - 1);
}

/**
 * Map a non-2xx body/status onto the tagged classes. Returns undefined when
 * the response is fine (2xx).
 */
export async function errorFromResponse(
  response: FetchResponseLike,
  bodyText: string | undefined,
): Promise<VoxixError | undefined> {
  if (response.ok) return undefined;
  let code: string | undefined;
  let message: string | undefined;
  let retryable: boolean | undefined;
  let details: unknown;
  if (bodyText) {
    try {
      const parsed = JSON.parse(bodyText) as VxErrorEnvelope;
      if (parsed && typeof parsed === "object" && "error" in parsed) {
        code = parsed.error.code;
        message = parsed.error.message;
        retryable = parsed.error.retryable;
        details = parsed.error.details;
      }
    } catch {
      /* not JSON — fall through to status mapping */
    }
  }
  const status = response.status;
  if (status === 401 || status === 403) {
    void details;
    return new AuthError({
      message: message ?? `authentication failed (${status})`,
      status,
      remoteCode: code,
      retryable: retryable ?? false,
    });
  }
  const init = {
    message: message ?? `service error ${status}${code ? ` (${code})` : ""}`,
    status,
    remoteCode: code,
    retryable: retryable ?? status >= 500,
    details,
  };
  if (code !== undefined && MODEL_ERROR_CODES.has(code)) {
    return new ModelError(init);
  }
  return new ServiceError(init);
}

function isAbort(cause: unknown): boolean {
  return cause instanceof Error && cause.name === "AbortError";
}

async function handleResponse<T>(response: FetchResponseLike): Promise<T> {
  if (response.ok) {
    if (response.status === 204) return undefined as T;
    const text = await response.text();
    if (text.length === 0) return undefined as T;
    return JSON.parse(text) as T;
  }
  const bodyText = await response.text().catch(() => undefined);
  const err = await errorFromResponse(response, bodyText);
  throw (
    err ??
    new ServiceError({
      message: `service error ${response.status}`,
      status: response.status,
      retryable: response.status >= 500,
    })
  );
}

/* ------------------------------------------------------------------ */
/* Client                                                              */
/* ------------------------------------------------------------------ */

export class VoxixClient {
  private baseUrl: string;
  private token: string | (() => Promise<string> | string);
  private fetchFn: Promise<FetchLike>;
  private factory?: WebSocketFactory;
  private ticketFallback: boolean;
  readonly timeouts: SessionTimeouts;

  constructor(options: VoxixClientOptions) {
    if (typeof options.baseUrl !== "string" || options.baseUrl.length === 0) {
      throw new ProtocolError({ message: "baseUrl is required" });
    }
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.token = options.token;
    this.timeouts = { ...DEFAULT_TIMEOUTS, ...(options.timeouts ?? {}) };
    this.ticketFallback = options.ticketFallback === true;
    this.factory = options.webSocketFactory;
    this.fetchFn = options.fetch ? Promise.resolve(options.fetch) : Promise.resolve().then(() => defaultFetch());
  }

  private async rawFetch(
    url: string,
    init: Parameters<FetchLike>[1],
    signal?: AbortSignalLike,
  ): Promise<FetchResponseLike> {
    const fetchImpl = await this.fetchFn;
    if (signal?.aborted) throw new CancelledError({ message: "aborted before the request" });
    try {
      return await fetchImpl(url, init);
    } catch (cause) {
      throw wrapTransportFailure(cause);
    }
  }

  /** Authenticated request helper with envelope mapping. */
  private async request<T>(
    method: string,
    path: string,
    opts?: { bodyBytes?: Uint8Array; bodyJson?: unknown; signal?: AbortSignalLike; extraHeaders?: Record<string, string> },
  ): Promise<T> {
    const token = await renderToken(this.token);
    const headers: Record<string, string> = { Authorization: `Bearer ${token}` };
    let body: Uint8Array | string | undefined;
    if (opts?.bodyBytes) {
      headers["Content-Type"] = "application/octet-stream";
      body = opts.bodyBytes;
    } else if (opts?.bodyJson !== undefined) {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(opts.bodyJson);
    }
    if (opts?.extraHeaders) Object.assign(headers, opts.extraHeaders);
    const response = await this.rawFetch(joinUrl(this.baseUrl, path), {
      method,
      headers,
      body,
      signal: opts?.signal,
    });
    try {
      return await handleResponse<T>(response);
    } catch (cause) {
      if (cause instanceof VoxixError) throw cause;
      throw new TransportError({
        message: cause instanceof Error ? cause.message : "response handling failed",
        retryable: true,
        cause,
      });
    }
  }

  /** GET /alive — unauthenticated bare liveness. */
  async alive(signal?: AbortSignalLike): Promise<boolean> {
    let response: FetchResponseLike;
    try {
      response = await this.rawFetch(joinUrl(this.baseUrl, "/alive"), { method: "GET", signal });
    } catch (cause) {
      throw wrapTransportFailure(cause);
    }
    if (!response.ok) return false;
    try {
      const parsed = (await response.json()) as { alive?: unknown };
      return parsed.alive === true;
    } catch {
      return false;
    }
  }

  async health(signal?: AbortSignalLike): Promise<Health> {
    return this.request("GET", "/v1/health", { signal });
  }

  async status(signal?: AbortSignalLike): Promise<ServerStatus> {
    return this.request("GET", "/v1/status", { signal });
  }

  /** GET /v1/models?installed=true|false. */
  async listModels(filter?: { installed?: boolean }, signal?: AbortSignalLike): Promise<ModelsPage> {
    const suffix = filter?.installed === undefined ? "" : `?installed=${filter.installed ? "true" : "false"}`;
    return this.request("GET", `/v1/models${suffix}`, { signal });
  }

  /** GET /v1/models/{id} — {model, prepare:{stage}}. */
  async getModel(id: string, signal?: AbortSignalLike): Promise<SingleModel> {
    return this.request("GET", `/v1/models/${encodeURIComponent(id)}`, { signal });
  }

  /** GET /v1/models/{id}/downloads — read-only download status projection. */
  async getDownload(id: string, signal?: AbortSignalLike): Promise<ModelDownloadStatus> {
    return this.request("GET", `/v1/models/${encodeURIComponent(id)}/downloads`, { signal });
  }

  /** PUT /v1/models/selected — selection with an explicit eager/lazy load. */
  async selectModel(id: string, load?: ModelSelectionLoad, signal?: AbortSignalLike): Promise<ModelSelection> {
    return this.request("PUT", "/v1/models/selected", {
      bodyJson: { model_id: id, load: load ?? "eager" },
      signal,
    });
  }

  /**
   * POST /v1/models/{id}/prepare — drives the SSE progress stream to a
   * terminal event, observing every stage via `onProgress` and resolving to
   * the terminal projection. A terminal `error` rejects with ModelError.
   */
  async prepareModel(
    id: string,
    onProgress?: (event: PrepareEvent) => void,
    signal?: AbortSignalLike,
  ): Promise<ModelDownload> {
    let terminal: PrepareEvent | undefined;
    for await (const event of this.prepareEvents(id, signal)) {
      onProgress?.(event);
      if (event.event === "ok" || event.event === "error") terminal = event;
    }
    if (!terminal) {
      throw new TransportError({
        message: "prepare stream ended without a terminal event",
        retryable: true,
      });
    }
    if (terminal.event === "error") {
      throw new ModelError({
        message: terminal.message,
        remoteCode: terminal.code,
        retryable: terminal.retryable,
      });
    }
    void terminal;
    if (terminal.event !== "ok") {
      throw new TransportError({
        message: "prepare stream ended in an unknown terminal state",
        retryable: true,
      });
    }
    return {
      state: "completed",
      downloaded_bytes: 0,
      total_bytes: 0,
      backend: terminal.backend,
    };
  }

  /** POST /v1/models/{id}/download — managed download (v1.1 additive).
   * Starts (or converges on) the single in-flight download: `200` when the
   * model is already installed, `202` when accepted/already in flight.
   * Progress is polled at `GET /v1/models/{id}/downloads` (`getDownload`). */
  async downloadModel(id: string, signal?: AbortSignalLike): Promise<ModelDownloadStatus> {
    return this.request("POST", `/v1/models/${encodeURIComponent(id)}/download`, { signal });
  }

  /** DELETE /v1/models/{id}/download — cancels an in-flight managed download
   * (v1.1 additive). Resolves on 204 (partials are kept for resume); throws
   * the tagged error classes otherwise (404 → ModelError, 422 nothing to
   * cancel → ServiceError, 503 unsupported → ServiceError). */
  async cancelModelDownload(id: string, signal?: AbortSignalLike): Promise<void> {
    return this.request("DELETE", `/v1/models/${encodeURIComponent(id)}/download`, { signal });
  }

  /** Raw prepare SSE tap (additive power method; `prepareModel` wraps it). */
  async *prepareEvents(id: string, signal?: AbortSignalLike): AsyncIterable<PrepareEvent> {
    const token = await renderToken(this.token);
    const response = await this.rawFetch(
      joinUrl(this.baseUrl, `/v1/models/${encodeURIComponent(id)}/prepare`),
      {
        method: "POST",
        headers: { Authorization: `Bearer ${token}`, Accept: "text/event-stream" },
      },
      signal,
    );
    if (!response.ok) {
      const bodyText = await response.text().catch(() => undefined);
      const failure = await errorFromResponse(response, bodyText);
      throw (
        failure ??
        new ServiceError({
          message: `prepare failed (${response.status})`,
          status: response.status,
          retryable: response.status >= 500,
        })
      );
    }
    const reader = response.body?.getReader();
    if (!reader) {
      throw new TransportError({
        message: "prepare response carried no stream body",
        status: response.status,
        retryable: false,
      });
    }

    // Incremental SSE over bytes: complete LF-terminated lines decode in
    // place, so multi-byte UTF-8 can never split at a chunk boundary.
    let eventName: string | undefined;
    let dataLines: string[] = [];
    const flushLine = (bytes: Uint8Array): PrepareEvent | undefined => {
      const line = decodeUtf8(stripCr(bytes));
      if (line.length === 0) {
        // Blank line = event boundary.
        const event = materializePrepareEvent(eventName, dataLines);
        eventName = undefined;
        dataLines = [];
        return event;
      }
      if (line.startsWith(":")) return undefined; // keep-alive comment
      const colon = line.indexOf(":");
      const field = colon < 0 ? line : line.slice(0, colon);
      const value = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
      if (field === "event") eventName = value;
      else if (field === "data") dataLines.push(value);
      return undefined; // fields accumulate until the blank line
    };

    const hold: Uint8Array[] = [];
    let streamFailure: unknown;
    try {
      for (;;) {
        const chunk = await reader.read();
        if (chunk.done) break;
        if (!chunk.value) continue;
        hold.push(chunk.value);
        if (signal?.aborted) throw new CancelledError({ message: "prepare aborted" });
        const whole = joinBytes(hold);
        hold.length = 0;
        let start = 0;
        for (let i = 0; i < whole.length; i++) {
          if (whole[i] !== 0x0a) continue;
          const event = flushLine(whole.subarray(start, i));
          start = i + 1;
          if (event) yield event;
        }
        if (start < whole.length) hold.push(whole.subarray(start));
      }
      // EOF flush: a terminal event may lack the trailing newline.
      if (hold.length > 0) {
        const event = flushLine(joinBytes(hold));
        hold.length = 0;
        if (event) yield event;
      }
    } catch (cause) {
      streamFailure = cause;
    } finally {
      await response.body?.cancel().catch(() => undefined);
    }
    if (streamFailure !== undefined) throw wrapTransportFailure(streamFailure);
  }

  /** POST /v1/sessions — returns a VoxixSession wired to the created creds. */
  async createSession(body: CreateSessionBody, signal?: AbortSignalLike): Promise<VoxixSession> {
    const created = await this.request<CreatedSession>("POST", "/v1/sessions", {
      bodyJson: body,
      signal,
    });
    return this.attachSession(created);
  }

  /** Rebinds to an existing created session (power path; resume flows). */
  attachSession(created: CreatedSession): VoxixSession {
    return new VoxixSession(this, created, {
      webSocketFactory: this.factory,
      ticketFallback: this.ticketFallback,
      helloMs: this.timeouts.helloMs,
      startedMs: this.timeouts.startedMs,
      finalMs: this.timeouts.finalMs,
      closeGraceMs: this.timeouts.closeGraceMs,
      pingIntervalMs: this.timeouts.pingIntervalMs,
    });
  }

  /** GET /v1/sessions/{id} — requires the per-session token header. */
  async getSession(sessionId: string, sessionToken: string, signal?: AbortSignalLike): Promise<SessionView> {
    return this.request("GET", `/v1/sessions/${encodeURIComponent(sessionId)}`, {
      signal,
      extraHeaders: { "X-Session-Token": sessionToken },
    });
  }

  /** POST /v1/transcriptions — raw WAV bytes; 202 with the job id. */
  async transcribeFile(
    file: Blob | ArrayBuffer,
    signal?: AbortSignalLike,
  ): Promise<BatchJob> {
    let bytes: Uint8Array;
    if (typeof Blob !== "undefined" && file instanceof Blob) {
      bytes = new Uint8Array(await file.arrayBuffer());
    } else {
      bytes = new Uint8Array(file as ArrayBuffer);
    }
    return this.request("POST", "/v1/transcriptions", { bodyBytes: bytes, signal });
  }

  async getTranscription(jobId: string, signal?: AbortSignalLike): Promise<BatchJobStatus> {
    return this.request("GET", `/v1/transcriptions/${encodeURIComponent(jobId)}`, { signal });
  }

  /** Best-effort cancel; resolves false on 404/409 ("already terminal"). */
  async cancelTranscription(jobId: string): Promise<boolean> {
    try {
      await this.request("DELETE", `/v1/transcriptions/${encodeURIComponent(jobId)}`);
      return true;
    } catch (error) {
      if (error instanceof ServiceError && (error.status === 404 || error.status === 409)) {
        return false;
      }
      throw error;
    }
  }

  /** GET /v1/history?cursor&limit. */
  async history(page?: { cursor?: number; limit?: number }, signal?: AbortSignalLike): Promise<HistoryPage> {
    const params = new URLSearchParams();
    if (page?.cursor !== undefined) params.set("cursor", String(page.cursor));
    if (page?.limit !== undefined) params.set("limit", String(page.limit));
    const qs = params.toString();
    return this.request("GET", `/v1/history${qs ? `?${qs}` : ""}`, { signal });
  }

  async config(signal?: AbortSignalLike): Promise<ServiceConfig> {
    return this.request("GET", "/v1/config", { signal });
  }

  async patchConfig(patch: ServiceConfigPatch, signal?: AbortSignalLike): Promise<ServiceConfig> {
    return this.request("PATCH", "/v1/config", { bodyJson: patch, signal });
  }

  /** POST /v1/auth/ws-ticket — one-use 60 s ticket for the WS upgrade. */
  async mintTicket(sessionId?: string, signal?: AbortSignalLike): Promise<WsTicket> {
    return this.request("POST", "/v1/auth/ws-ticket", {
      bodyJson: sessionId ? { session_id: sessionId } : {},
      signal,
    });
  }

  /* -- internal seams used by VoxixSession --------------------------- */

  /** @internal */
  internal_wsFactory(): WebSocketFactory {
    return this.factory ?? defaultWebSocketFactory();
  }

  /** @internal */
  internal_baseUrl(): string {
    return this.baseUrl;
  }

  /** @internal */
  internal_bearer(): Promise<string> {
    return renderToken(this.token);
  }

  /** @internal */
  internal_ticket(): Promise<string | undefined> {
    if (!this.ticketFallback) return Promise.resolve(undefined);
    return this.mintTicket().then((t) => t.ticket);
  }
}

/** Parse an SSE (event, data) pair restricted to the prepare vocabulary. */
function materializePrepareEvent(
  event: string | undefined,
  dataLines: string[],
): PrepareEvent | undefined {
  if (!event || dataLines.length === 0) return undefined;
  let payload: Record<string, unknown>;
  try {
    payload = JSON.parse(dataLines.join("\n")) as Record<string, unknown>;
  } catch {
    return undefined;
  }
  switch (event) {
    case "verifying":
      return { event: "verifying", model_id: String(payload.model_id ?? "") };
    case "downloading":
      return {
        event: "downloading",
        model_id: String(payload.model_id ?? ""),
        downloaded_bytes: Number(payload.downloaded_bytes ?? 0),
        total_bytes: Number(payload.total_bytes ?? 0),
      };
    case "loading":
      return { event: "loading", model_id: String(payload.model_id ?? "") };
    case "ok":
      return {
        event: "ok",
        model_id: String(payload.model_id ?? ""),
        backend: String(payload.backend ?? ""),
      };
    case "error":
      return {
        event: "error",
        code: String(payload.code ?? "engine_error"),
        message: String(payload.message ?? "prepare failed"),
        retryable: payload.retryable === true,
      };
    default:
      return undefined; // additive events are ignored
  }
}
