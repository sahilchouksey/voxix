/**
 * VoxixSession — one WS media-plane session (protocol v1 §6/§11, as
 * implemented by the service):
 *
 *   connect() → hello         (barrier: no start before hello)
 *   start()   → started       (barrier: no audio before it)
 *   pushAudio(frame, ms)      (§6 binary frames, client seq from 0)
 *   pause()   → paused        (the audio-acceptance barrier)
 *   resume()  → wire `resume` (no wire event; the tap gets a synthesized one)
 *   stop()    → final (exactly one), then the server closes 1000
 *   cancel()  → results suppressed, server closes 1000 — no final ever
 *   close()   → local teardown (idempotent)
 *
 * `events` is a bounded drop-oldest single-consumer AsyncIterable tap
 * (§15 G25): a second concurrent consumer errors strictly.
 *
 * Ordering: every pushAudio validates, encodes, and sends synchronously
 * before its promise resolves (any bufferedAmount backpressure happens
 * strictly after the send), so session controls called later are ordered
 * after all prior frames — the FIFO discipline the server's stop/pause
 * barriers rely on.
 */
import {
  AuthError,
  CancelledError,
  ProtocolError,
  ServiceError,
  TransportError,
  VoxixError,
  wrapTransportFailure,
} from "./errors.js";
import {
  CreatedSession,
  Envelope,
  ErrorPayload,
  FinalResult,
  HelloPayload,
  SessionEvent,
  StartedPayload,
} from "./protocol/index.js";
import {
  encodeAudioFrame,
  FrameDurationMs,
  samplesFor,
} from "./protocol/audio.js";
import {
  AbortSignalLike,
  defaultWebSocketFactory,
  WebSocketFactory,
  WebSocketLike,
} from "./transport.js";
import { BoundedTap } from "./async-queue.js";

/* ------------------------------------------------------------------ */

export type SessionPhase =
  | "new"
  | "connecting"
  | "connected"
  | "starting"
  | "streaming"
  | "paused"
  | "stopped"
  | "cancelled"
  | "failed"
  | "closed";

/** start() additions: optional M3 resume fencing pair (echoed in `start`). */
export interface SessionStartOptions {
  resume?: { resume_token: string; resume_epoch: number };
}

export interface ResumeCredentials {
  resume_token: string;
  resume_epoch: number;
}

export interface VoxixSessionConfig {
  webSocketFactory?: WebSocketFactory;
  /** One-use `?ticket=` upgrade instead of the bearer header (client option). */
  ticketFallback?: boolean;
  /** Hello barrier timeout (ms). */
  helloMs?: number;
  /** started barrier timeout (model load can take seconds on cold starts). */
  startedMs?: number;
  /** stop→final timeout (ms). */
  finalMs?: number;
  /** cancel grace: server close after the control (ms). */
  closeGraceMs?: number;
  /** App-level ping cadence (§15 S5 liveness; also refreshes paused
   * budgets). */
  pingIntervalMs?: number;
}

export type VoxixSessionOptions = VoxixSessionConfig;

/** Bounded-buffer backpressure thresholds for pushAudio (bufferedAmount). */
const HIGH_WATER_BYTES = 512 * 1024;
const LOW_WATER_BYTES = 128 * 1024;

const TERMINAL_PHASES: SessionPhase[] = ["stopped", "cancelled", "failed", "closed"];

/* ------------------------------------------------------------------ */

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(reason: unknown): void;
}

function createDeferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

interface XSessionToken extends CreatedSession {
  /* created credentials carry in full */
}

/* ------------------------------------------------------------------ */

export class VoxixSession {
  readonly id: string;
  /** Per-session sub-credential (carried in the 201 body). */
  readonly sessionToken: string;
  /** The bounded drop-oldest single-consumer observation tap. */
  readonly events: BoundedTap<SessionEvent>;

  private readonly client: {
    internal_baseUrl(): string;
    internal_bearer(): Promise<string>;
    internal_ticket(): Promise<string | undefined>;
  };
  private readonly created: CreatedSession;
  private readonly factory?: WebSocketFactory;
  private readonly ticketFallback: boolean;
  private readonly helloMs: number;
  private readonly startedMs: number;
  private readonly finalMs: number;
  private readonly closeGraceMs: number;
  private readonly pingIntervalMs: number;

  private ws: WebSocketLike | undefined;
  private phase: SessionPhase = "new";
  private nonce = 0;
  private audioSeq = 0;
  private pingTimer: ReturnType<typeof setInterval> | undefined;

  private lastHello: HelloPayload | undefined;
  private lastResume: ResumeCredentials | undefined;
  private lastAckThroughSeq = 0;

  private connectWaiter: Deferred<HelloPayload> | undefined;
  private startWaiter: Deferred<void> | undefined;
  private pauseWaiter: Deferred<number> | undefined;
  private stopWaiter: Deferred<FinalResult> | undefined;
  private cancelWaiter: Deferred<void> | undefined;

  constructor(
    client: VoxixSession["client"],
    created: CreatedSession,
    config: VoxixSessionConfig = {},
  ) {
    this.client = client;
    this.created = created;
    this.id = created.session_id;
    this.sessionToken = created.session_token;
    this.events = new BoundedTap<SessionEvent>(512);
    this.factory = config.webSocketFactory;
    this.ticketFallback = config.ticketFallback === true;
    this.helloMs = config.helloMs ?? 10_000;
    this.startedMs = config.startedMs ?? 30_000;
    this.finalMs = config.finalMs ?? 60_000;
    this.closeGraceMs = config.closeGraceMs ?? 5_000;
    this.pingIntervalMs = config.pingIntervalMs ?? 5_000;
  }

  /* -- observation --------------------------------------------------- */

  get phaseName(): SessionPhase {
    return this.phase;
  }

  get negotiatedAudio(): CreatedSession["negotiated_audio"] {
    return this.created.negotiated_audio;
  }

  get streamingMode(): string {
    return this.created.streaming_mode;
  }

  /** Mutable session facts after `started` (M3 fencing pair). */
  get resumeCredentials(): ResumeCredentials | undefined {
    return this.lastResume;
  }

  get lastAck(): { through_seq: number } {
    return { through_seq: this.lastAckThroughSeq };
  }

  get isTerminal(): boolean {
    return TERMINAL_PHASES.includes(this.phase);
  }

  /* -- lifecycle ------------------------------------------------------ */

  /** Open the media socket and await the hello barrier. */
  async connect(signal?: AbortSignalLike): Promise<void> {
    if (this.phase === "connecting" && this.connectWaiter) {
      await this.connectWaiter.promise;
      return;
    }
    if (this.phase !== "new") {
      throw new ProtocolError({ message: `connect() is invalid in phase ${this.phase}` });
    }
    const waiter = createDeferred<HelloPayload>();
    this.connectWaiter = waiter;
    this.phase = "connecting";
    void this.openSocket(signal);
    try {
      await this.raceTimeout(waiter.promise, this.helloMs, "hello", signal);
      if (this.phase === "connecting") this.phase = "connected";
    } finally {
      this.connectWaiter = undefined;
    }
  }

  /**
   * Send the start barrier and await `started` — the model snapshot, the
   * backend, and the M3 resume pair (minted on first start; rotated on a
   * resumed start) arrive with it.
   */
  async start(options?: SessionStartOptions): Promise<void> {
    if (this.phase === "starting" && this.startWaiter) {
      await this.startWaiter.promise;
      return;
    }
    if (this.phase !== "connected") {
      throw new ProtocolError({
        message: `start() is invalid in phase ${this.phase} (connect() first)`,
      });
    }
    const waiter = createDeferred<void>();
    this.startWaiter = waiter;
    this.phase = "starting";
    this.sendJson({
      type: "start",
      payload: options?.resume
        ? {
            resume_token: options.resume.resume_token,
            resume_epoch: options.resume.resume_epoch,
          }
        : {},
    });
    try {
      await this.raceTimeout(waiter.promise, this.startedMs, "started");
      if (this.phase === "starting") this.phase = "streaming";
      this.armPings();
    } catch (cause) {
      if (this.phase === "starting") this.phase = "connected";
      throw cause;
    } finally {
      this.startWaiter = undefined;
    }
  }

  /**
   * Push one audio frame. `frame` is mono PCM16 (Int16Array samples, or LE
   * bytes as Uint8Array) sized exactly for `durationMs` at 16 kHz: 160/320/
   * 480/640/960 samples for 10/20/30/40/60 ms. The frame is placed on the
   * wire synchronously; the promise then observes bounded-buffer
   * backpressure (strictly post-send, keeping call ordering FIFO).
   */
  async pushAudio(frame: Int16Array | Uint8Array, durationMs: FrameDurationMs): Promise<void> {
    if (!isLegalDuration(durationMs)) {
      throw new ProtocolError({ message: `durationMs ${durationMs} must be one of 10|20|30|40|60` });
    }
    if (this.phase !== "streaming") {
      throw new ProtocolError({ message: `pushAudio is invalid in phase ${this.phase}` });
    }
    if (this.negotiatedAudio.codec === "opus") {
      // The v1 SDK client encodes pcm_s16le frames only; the negotiated opus
      // codec needs a native Opus encoder (framing helper: `encodeOpusFrame`).
      throw new ProtocolError({
        message:
          "pushAudio sends pcm_s16le frames; the negotiated opus codec requires a native Opus encoder (use encodeOpusFrame)",
      });
    }
    const socket = this.ws;
    if (!socket || socket.readyState !== 1) {
      throw new TransportError({ message: "the media socket is not open" });
    }
    const expected = samplesFor(durationMs);
    const sampleCount = frame instanceof Int16Array ? frame.length : Math.floor(frame.length / 2);
    if (sampleCount !== expected) {
      throw new ProtocolError({
        message: `payload length does not match duration: expected ${expected} samples, got ${sampleCount}`,
      });
    }
    const wire = encodeAudioFrame(frame, durationMs, this.audioSeq);
    socket.send(wire);
    this.audioSeq++;
    if (socket.bufferedAmount !== undefined && socket.bufferedAmount >= HIGH_WATER_BYTES) {
      const deadline = Date.now() + this.finalMs;
      while (
        socket.bufferedAmount > LOW_WATER_BYTES &&
        Date.now() < deadline &&
        this.phase === "streaming"
      ) {
        await delay(8);
      }
    }
  }

  /** The audio-acceptance barrier: no frame may be sent after this
   * synchronously-ordered control (client discipline mirrors the wire's
   * post-pause 1008). Resolves with `paused.last_audio_seq`. */
  async pause(): Promise<number> {
    if (this.phase !== "streaming") {
      throw new ProtocolError({ message: `pause() is invalid in phase ${this.phase}` });
    }
    const waiter = createDeferred<number>();
    this.pauseWaiter = waiter;
    this.phase = "paused"; // reject new pushAudio immediately
    this.sendJson({ type: "pause", payload: {} });
    try {
      const through = await this.raceTimeout(waiter.promise, this.finalMs, "paused");
      this.phase = "paused";
      this.events.push({ type: "paused", payload: { last_audio_seq: through } });
      return through;
    } catch (cause) {
      if (this.phase === "paused") this.phase = "streaming";
      throw cause;
    } finally {
      this.pauseWaiter = undefined;
    }
  }

  /** Reopen audio acceptance. The wire carries no event; the tap observes a
   * client-synthesized `resumed` (documented). */
  async resume(): Promise<void> {
    if (this.phase !== "paused") {
      throw new ProtocolError({ message: `resume() is invalid in phase ${this.phase}` });
    }
    this.sendJson({ type: "resume", payload: {} });
    this.phase = "streaming";
    this.events.push({
      type: "resumed",
      payload: { last_audio_seq: Math.max(0, this.audioSeq - 1) },
    });
  }

  /** FIFO finalize barrier: every previously pushed frame is fed before the
   * decode; resolves with the exactly-one `final`. */
  async stop(reason?: "user" | "vad" | "end_of_stream"): Promise<FinalResult> {
    if (this.phase !== "streaming") {
      throw new ProtocolError({ message: `stop() is invalid in phase ${this.phase}` });
    }
    const waiter = createDeferred<FinalResult>();
    this.stopWaiter = waiter;
    this.sendJson({ type: "stop", payload: { reason: reason ?? "user" } });
    try {
      return await this.raceTimeout(waiter.promise, this.finalMs, "final");
    } finally {
      this.stopWaiter = undefined;
    }
  }

  /** Suppress results (G5): no final ever arrives; resolves when the
   * service closes the socket or the bounded grace elapses. */
  async cancel(): Promise<void> {
    if (this.cancelWaiter) {
      await this.cancelWaiter.promise;
      return;
    }
    if (this.isTerminal) {
      if (this.phase === "cancelled" || this.phase === "closed") return;
      throw new ProtocolError({ message: `cancel() is invalid in phase ${this.phase}` });
    }
    if (this.ws === undefined && (this.phase === "connecting" || this.phase === "new")) {
      // Nothing to cancel on the wire; treat as a local close.
      this.close();
      return;
    }
    const waiter = createDeferred<void>();
    this.cancelWaiter = waiter;
    try {
      this.sendJson({ type: "cancel", payload: { reason: "user" } });
    } catch (cause) {
      // A dead socket already tears down via its close handler.
      waiter.resolve();
    }
    try {
      await this.raceTimeout(
        waiter.promise,
        this.finalMs + this.closeGraceMs,
        "cancel",
      );
    } finally {
      this.cancelWaiter = undefined;
    }
  }

  /** Local teardown (idempotent). In-flight waiters reject with
   * CancelledError; the socket closes 1000; the tap drains and ends. */
  close(): void {
    if (this.phase === "closed") return;
    const wasTerminal = this.isTerminal;
    this.phase = "closed";
    this.teardownSocket();
    const abort = new CancelledError({
      message: wasTerminal ? "session already terminal" : "session closed locally",
    });
    this.connectWaiter?.reject(abort);
    this.connectWaiter = undefined;
    this.startWaiter?.reject(abort);
    this.startWaiter = undefined;
    this.pauseWaiter?.reject(abort);
    this.pauseWaiter = undefined;
    this.stopWaiter?.reject(abort);
    this.stopWaiter = undefined;
    this.cancelWaiter?.resolve();
    this.cancelWaiter = undefined;
    this.events.close();
  }

  /* -- socket plumbing ------------------------------------------------- */

  private async openSocket(signal?: AbortSignalLike): Promise<void> {
    let url = toWsUrl(this.client.internal_baseUrl(), this.created.ws_url);
    const headers: Record<string, string> = {
      "X-Session-Token": this.sessionToken,
    };
    try {
      if (this.ticketFallback) {
        const ticket = await this.client.internal_ticket();
        if (typeof ticket === "string" && ticket.length > 0) {
          url += (url.includes("?") ? "&" : "?") + `ticket=${encodeURIComponent(ticket)}`;
        }
      } else {
        headers.Authorization = `Bearer ${await this.client.internal_bearer()}`;
      }
    } catch (cause) {
      this.failSync(wrapTransportFailure(cause));
      return;
    }

    if (signal?.aborted) {
      this.failSync(new CancelledError({ message: "connect aborted" }));
      return;
    }
    let socket: WebSocketLike;
    try {
      const factory: WebSocketFactory = this.factory ?? defaultWebSocketFactory();
      socket = factory(url, { headers });
    } catch (cause) {
      this.failSync(wrapTransportFailure(cause));
      return;
    }
    this.ws = socket;
    socket.onmessage = (message) => this.onMessage(message.data);
    socket.onerror = () => {
      /* the close handler is the authoritative terminal */
    };
    socket.onclose = (event) => this.onClose(event.code, event.reason);
    if (signal) {
      signal.addEventListener("abort", () => {
        if (this.isTerminal) return;
        // Local abort: close the socket; the service tears the session down
        // (M2 §7 socket-death semantics) — waiters reject cancelled.
        try {
          socket.onclose?.({ code: 1000, reason: "aborted" });
          socket.close(1000, "aborted");
        } catch {
          /* socket already closed */
        }
        this.abortLocal();
      });
    }
  }

  /** Abort path: reject waiters with CancelledError, mark cancelled. */
  private abortLocal(): void {
    const cancel = createDeferred<void>();
    this.cancelWaiter = cancel;
    this.cancelWaiter.resolve(undefined);
    cancel.resolve();
    this.phase = "cancelled";
    this.cancelWaiter.resolve();
    const abort = new CancelledError({ message: "aborted" });
    this.connectWaiter?.reject(abort);
    this.connectWaiter = undefined;
    this.startWaiter?.reject(abort);
    this.startWaiter = undefined;
    this.pauseWaiter?.reject(abort);
    this.pauseWaiter = undefined;
    this.stopWaiter?.reject(abort);
    this.stopWaiter = undefined;
    this.cancelWaiter.resolve();
    this.cancelWaiter = undefined;
    this.events.close();
  }

  private onMessage(raw: string | ArrayBuffer | Uint8Array): void {
    if (this.phase === "closed") return;
    if (typeof raw !== "string") return; // the service sends no binary events
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      this.failSync(new ProtocolError({ message: "inbound frame is not JSON" }));
      return;
    }
    if (!isEnvelope(parsed)) {
      this.failSync(new ProtocolError({ message: "inbound frame is not a v1 envelope" }));
      return;
    }
    const envelope = parsed as Envelope<never>;
    const type = envelope.type;
    const payload = envelope.payload as Record<string, unknown>;
    switch (type) {
      case "hello":
        {
          this.lastHello = payload as unknown as HelloPayload;
          const waiter = this.connectWaiter;
          this.connectWaiter = undefined;
          waiter?.resolve(this.lastHello);
          this.events.push({ type: "hello", payload: this.lastHello });
        }
        return;
      case "started":
        {
          const started = payload as unknown as StartedPayload;
          this.lastResume = { resume_token: started.resume_token, resume_epoch: started.resume_epoch };
          const waiter = this.startWaiter;
          this.startWaiter = undefined;
          waiter?.resolve(undefined);
          this.events.push({ type: "started", payload: started });
        }
        return;
      case "paused":
        {
          const through = Number(payload.last_audio_seq ?? 0);
          const waiter = this.pauseWaiter;
          this.pauseWaiter = undefined;
          waiter?.resolve(through);
          // The tap 'paused' event is pushed by pause() when its await
          // settles (single-consumer tap ordering equals API order).
        }
        return;
      case "speech_start":
        this.events.push({ type: "speech_start", payload: payload as never });
        return;
      case "speech_end":
        this.events.push({ type: "speech_end", payload: payload as never });
        return;
      case "audio_ack":
        this.lastAckThroughSeq = Number(payload.through_seq ?? 0);
        this.events.push({ type: "audio_ack", payload: payload as never });
        return;
      case "partial":
        this.events.push({ type: "partial", payload: payload as never });
        return;
      case "finalizing":
        this.events.push({ type: "finalizing", payload: payload as never });
        return;
      case "final":
        {
          const result = payload as unknown as FinalResult;
          this.phase = "stopped";
          const waiter = this.stopWaiter;
          this.stopWaiter = undefined;
          this.events.push({ type: "final", payload: result });
          waiter?.resolve(result);
        }
        return;
      case "error":
        this.onErrorEnvelope(payload as unknown as ErrorPayload);
        return;
      case "pong":
        return; // app-level heartbeat echo; not surfaced
      default:
        // Unsupported `v` closes 4002 server-side; unknown types here are
        // additive server events — ignored (wire-evolution rule).
        return;
    }
  }

  /** Error envelope: fatal → fail the session; retryable → reject an
   * in-flight start contention without tearing down the turn. */
  private onErrorEnvelope(payload: ErrorPayload): void {
    const error = new ServiceError({
      message: payload.message,
      remoteCode: payload.code,
      retryable: payload.retryable === true,
    });
    this.events.push({ type: "error", payload });
    if (payload.retryable === true) {
      const waiter = this.startWaiter;
      this.startWaiter = undefined;
      if (waiter) {
        this.phase = "connected";
        waiter.reject(error);
      }
      return;
    }
    if (this.phase === "streaming" || this.phase === "starting" || this.phase === "connected") {
      const waiter = this.startWaiter;
      this.startWaiter = undefined;
      waiter?.reject(error);
      this.failSync(error);
    } else if (this.stopWaiter) {
      const waiter = this.stopWaiter;
      this.stopWaiter = undefined;
      waiter.reject(error);
    }
  }

  private onClose(code: number, reason: string): void {
    if (this.phase === "closed") return;
    this.ws = undefined;
    this.pingTimerClear();
    const hadFinal = this.phase === "stopped";
    const cancelWaiter = this.cancelWaiter;
    this.cancelWaiter = undefined;

    if (hadFinal) {
      // The planned terminal: final already resolved; the stream closes 1000.
      this.events.close();
      return;
    }
    if (cancelWaiter) {
      cancelWaiter.resolve(undefined);
      this.phase = "cancelled";
      this.events.close();
      return;
    }

    const mapped = closeError(code, reason);
    this.phase = "failed";
    const connectWaiter = this.connectWaiter;
    const startWaiter = this.startWaiter;
    const pauseWaiter = this.pauseWaiter;
    const stopWaiter = this.stopWaiter;
    this.connectWaiter = undefined;
    this.startWaiter = undefined;
    this.pauseWaiter = undefined;
    this.stopWaiter = undefined;
    connectWaiter?.reject(mapped);
    startWaiter?.reject(mapped);
    pauseWaiter?.reject(mapped);
    stopWaiter?.reject(mapped);
    this.events.fail(mapped);
  }

  /** Terminal failure without socket close (malformed frame, etc.). */
  private failSync(cause: VoxixError): void {
    if (this.isTerminal) return;
    this.phase = "failed";
    const connectWaiter = this.connectWaiter;
    const startWaiter = this.startWaiter;
    const pauseWaiter = this.pauseWaiter;
    const stopWaiter = this.stopWaiter;
    this.connectWaiter = undefined;
    this.startWaiter = undefined;
    this.pauseWaiter = undefined;
    this.stopWaiter = undefined;
    connectWaiter?.reject(cause);
    startWaiter?.reject(cause);
    pauseWaiter?.reject(cause);
    stopWaiter?.reject(cause);
    this.teardownSocket();
    this.events.fail(cause);
  }

  private teardownSocket(): void {
    this.pingTimerClear();
    if (this.ws) {
      const socket = this.ws;
      this.ws = undefined;
      try {
        socket.onclose = null;
        socket.onmessage = null;
        socket.onerror = null;
        socket.onopen = null;
        if (socket.readyState === 0 || socket.readyState === 1) socket.close(1000, "local close");
      } catch {
        /* socket already gone */
      }
    }
  }

  /** App-level ping loop (§15 S5: pings refresh paused/idle liveness). */
  private armPings(): void {
    this.pingTimerClear();
    this.pingTimer = setInterval(() => {
      if (this.ws && this.ws.readyState === 1 && !TERMINAL_PHASES.includes(this.phase)) {
        this.nonce += 1;
        const n = this.nonce;
        try {
          this.ws.send(JSON.stringify({ type: "ping", payload: { nonce: n } }));
        } catch {
          /* the close path surfaces transport failures */
        }
      }
    }, this.pingIntervalMs);
  }

  private pingTimerClear(): void {
    if (this.pingTimer !== undefined) {
      clearInterval(this.pingTimer);
      this.pingTimer = undefined;
    }
  }

  private sendJson(message: { type: string; payload: unknown }): void {
    const socket = this.ws;
    if (!socket || socket.readyState !== 1) {
      throw new TransportError({ message: "the media socket is not open" });
    }
    socket.send(JSON.stringify(message));
  }

  private async raceTimeout<T>(
    promise: Promise<T>,
    timeoutMs: number,
    what: string,
    signal?: AbortSignalLike,
  ): Promise<T> {
    return await new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(new TransportError({ message: `${what} timed out after ${timeoutMs}ms`, retryable: true }));
      }, timeoutMs);
      promise.then(
        (value) => {
          clearTimeout(timer);
          if (signal?.aborted) reject(new CancelledError({ message: `${what} aborted` }));
          else resolve(value);
        },
        (cause) => {
          clearTimeout(timer);
          reject(cause);
        },
      );
    });
  }
}

/* ------------------------------------------------------------------ */

function isLegalDuration(value: FrameDurationMs): value is FrameDurationMs {
  return value === 10 || value === 20 || value === 30 || value === 40 || value === 60;
}

function isEnvelope(value: unknown): value is Envelope<unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as Envelope).v === "number" &&
    typeof (value as Envelope).type === "string" &&
    typeof (value as Envelope).payload === "object"
  );
}

function toWsUrl(baseUrl: string, wsPath: string): string {
  const http = baseUrl.replace(/\/+$/, "") + (wsPath.startsWith("/") ? wsPath : `/${wsPath}`);
  return http.replace(/^http:\/\//, "ws://").replace(/^https:\/\//, "wss://");
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Map a WS close code onto the tagged errors (§9 close-code table). */
export function closeError(
  code: number,
  reason: string,
): TransportError | AuthError | ProtocolError | ServiceError {
  const message = reason.trim().length > 0 ? reason : `media socket closed ${code}`;
  switch (code) {
    case 1000:
      return new TransportError({
        message: `closed 1000 without a terminal event (${message})`,
      });
    case 1001:
      return new TransportError({ message: `daemon shut down (${message})`, retryable: true });
    case 1013:
      return new ServiceError({
        message: `overloaded: outbound backpressure or admission refused (${message})`,
        remoteCode: "service_full",
        retryable: true,
      });
    case 1011:
      return new ServiceError({
        message: `engine failure closed the stream (${message})`,
        remoteCode: "engine_error",
        retryable: true,
      });
    case 4001:
      return new AuthError({
        message: `bearer token rotated (${message}) — reconnect with a fresh token`,
        remoteCode: "token_rotated",
        retryable: true,
      });
    case 4002:
      return new ProtocolError({ message: `protocol version mismatch (${message})` });
    case 4003:
      return new ServiceError({
        message: `resume credentials stale or expired (${message}) — reconnect with a fresh session`,
        remoteCode: "stale_resume",
        retryable: false,
      });
    case 4004:
      return new ProtocolError({ message: `audio sequence gap (${message})` });
    case 1008:
      return new ProtocolError({ message: `policy violation (${message})` });
    default:
      return new ProtocolError({ message: `unexpected close code ${code} (${message})` });
  }
}

export type { XSessionToken };
