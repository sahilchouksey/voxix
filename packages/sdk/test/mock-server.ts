/**
 * Offline test harness: a lightweight fake voxix daemon — REST control plane
 * (real handlers per protocol-v1 as implemented through M3) plus the WS
 * media plane — exercised by the SDK's real client/session code over real
 * HTTP/WebSocket. No network stubs.
 *
 * Behaviors mirrored from the service (deterministic engine):
 * - bearer on every /v1 route; 401 empty body; `?ticket=` admits /stream
 *   upgrades when the bearer is absent (one-use, 60 s);
 * - session `X-Session-Token` — wrong/missing answers 404 session_not_found;
 * - second socket attach → 503 service_full Retry-After 1;
 * - WS: hello → started (resume pair minted/rotated; stale pair → error
 *   `stale_resume` + close 4003) → §6 frames validated (12-byte header,
 *   gaps → 4004) → pausable/„resumable → stop {reason} final exactly-once +
 *   close 1000 → cancel suppresses results (no final);
 * - prepare SSE verifying→loading→ok (or terminal error for a bad model);
 *   409 prepare-busy on a second concurrent prepare;
 * - batch POST raw WAV → 202; GET completes after a tick; DELETE 204;
 * - history page; config GET/PATCH; token WS-ticket minting.
 *
 * Test-only extension: `harnessOptions.keepAliveOnClose` drops a streaming
 * socket without tearing the session down (the M3 reconnect-within-window
 * path the daemon's teardown otherwise consumes).
 */
import { SAMPLE_RATE_HZ } from "../src/protocol/audio.js";

export interface FakeModel {
  id: string;
  name?: string;
  /** `POST /v1/models/{id}/prepare` streams a terminal error when set. */
  prepareError?: { code: string; message: string; retryable: boolean };
  /** Defaults to installed; `false` exposes a not-yet-installed catalog entry
   * for the managed-download routes (`POST/DELETE .../download`). */
  installed?: boolean;
}

export interface HarnessOptions {
  label?: string;
  /** Deterministic transcript used by partials/finals. */
  transcript?: string;
  /** Models exposed by the fake catalog (a default set applies). */
  models?: FakeModel[];
  /** Test-only: socket close does NOT cancel the session. */
  keepAliveOnClose?: boolean;
}

export interface Harness {
  baseUrl: string;
  token: string;
  port: number;
  stop(): Promise<void>;
  sessionState(id: string): SessionSnapshot | undefined;
  allSessionStates(): SessionSnapshot[];
  cancelSession(id: string): void;
  /** Occupy the prepare slot; returns the release fn (prepare-busy probe). */
  holdPrepare(): () => void;
}

export interface SessionSnapshot {
  id: string;
  state: string;
  audioReceivedMs: number;
  lastAudioSeq: number;
  lastEventSeq: number;
  final?: unknown;
  error?: unknown;
}

interface SessionState {
  id: string;
  token: string;
  state: "created" | "completed" | "cancelled" | "failed";
  createdAt: number;
  clientKey?: string;
  modelId?: string;
  audioReceivedMs: number;
  lastAudioSeq: number;
  lastEventSeq: number;
  final?: unknown;
  error?: unknown;
  /** M3 fencing pair (session-lifetime, rotated on each valid resume). */
  resumeToken?: string;
  resumeEpoch?: number;
  /** True once the first audio sequence has been accepted (the §11.4
   * duplicate rule applies per-sequence from then on). */
  sawAudio: boolean;
}

interface SocketShim {
  send(data: string | Uint8Array): void;
  close(code?: number, reason?: string): void;
}

interface StreamTask {
  sessionId: string;
  phase: "awaiting_start" | "streaming" | "paused" | "finalizing" | "terminal";
  seq: number;
  framesSeen: number;
  partialRevision: number;
  socket: SocketShim;
}

interface TicketState {
  session?: string;
  expiresAtMs: number;
  consumed: boolean;
}

let harnessCounter = 0;

export function startHarness(options: HarnessOptions = {}): Promise<Harness> {
  harnessCounter += 1;
  const harnessId = harnessCounter;
  const primaryToken = `vx_h${harnessId}_service_${Math.random().toString(36).slice(2, 10)}`;
  const models: FakeModel[] = options.models ?? [
    { id: "tiny-16k", name: "Tiny Speech 16k" },
    { id: "missing-model", name: "Absent Model", prepareError: { code: "engine_error", message: "model load failed", retryable: true } },
    { id: "catalog-large", name: "Catalog Large", installed: false },
  ];
  const sessions = new Map<string, SessionState>();
  const sessionOrder: string[] = [];
  const tickets = new Map<string, TicketState>();
  const prepareSlot = { held: false };
  /** In-flight managed downloads (models whose install has started but
   * not canceled): the `POST/DELETE .../download` routes converge on this. */
  const inflightDownloads = new Set<string>();
  let selectedModelId = "tiny-16k";
  let loadedModelId = "tiny-16k";
  const jobs = new Map<string, { state: string; result?: unknown }>();
  const history: Array<Record<string, unknown>> = [];
  const taskBysession = new Map<string, StreamTask>();

  const envelope = (code: string, message: string, retryable: boolean) => ({
    error: { code, message, retryable, request_id: `req-${Math.random().toString(36).slice(2)}` },
  });
  const json = (status: number, body: unknown, extraHeaders?: Record<string, string>) =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": "application/json", ...(extraHeaders ?? {}) },
    });

  const nowMs = () => Date.now();
  const randomToken = (prefix: string) => `${prefix}${Math.random().toString(36).slice(2, 18)}`;

  /* ------------------------------------------------------------ */
  /* WS media-plane task                                           */
  /* ------------------------------------------------------------ */

  function sendEvent(task: StreamTask, type: string, payload: unknown): void {
    const session = sessions.get(task.sessionId);
    const wire = {
      v: 1,
      type,
      session_id: task.sessionId,
      seq: task.seq++,
      ts_ms: nowMs(),
      payload,
    };
    task.socket.send(JSON.stringify(wire));
    if (session) session.lastEventSeq = wire.seq;
  }

  function failTask(task: StreamTask, code: string, message: string, retryable: boolean, closeCode: number): void {
    sendEvent(task, "error", { code, message, retryable, details: null });
    const session = sessions.get(task.sessionId);
    if (session) {
      session.state = "failed";
      session.error = { code, message, retryable };
    }
    task.phase = "terminal";
    try {
      task.socket.close(closeCode, code);
    } catch {
      /* already closed */
    }
  }

  function dispatchControl(task: StreamTask, type: string, payload: Record<string, unknown>): void {
    const session = sessions.get(task.sessionId);
    if (!session) return;
    switch (type) {
      case "ping": {
        sendEvent(task, "pong", { nonce: Number(payload.nonce ?? 0) });
        return;
      }
      case "start": {
        if (task.phase !== "awaiting_start") {
          sendEvent(task, "error", {
            code: "service_busy",
            message: "an audio stream is already starting",
            retryable: true,
            details: null,
          });
          return;
        }
        // M3 fencing: presented resume creds validate against the session's
        // stored pair; stale or presented-before-any-start is the terminal
        // `stale_resume` error + close 4003.
        const presented = payload as { resume_token?: string; resume_epoch?: number };
        const hasCreds =
          presented.resume_token !== undefined || presented.resume_epoch !== undefined;
        if (hasCreds) {
          const ok =
            session.resumeToken !== undefined &&
            presented.resume_token === session.resumeToken &&
            presented.resume_epoch === session.resumeEpoch;
          if (!ok) {
            failTask(task, "stale_resume", "resume credentials do not match the live session; reconnect fresh", false, 4003);
            return;
          }
          session.resumeToken = randomToken("vx_res_");
          session.resumeEpoch = (presented.resume_epoch ?? 0) + 1;
        } else {
          session.resumeToken = randomToken("vx_res_");
          session.resumeEpoch = 1;
        }
        task.phase = "streaming";
        sendEvent(task, "started", {
          model_id: session.modelId ?? selectedModelId,
          backend: "transcribe_cpp",
          resumed: false,
          next_audio_seq: 0,
          resume_token: session.resumeToken,
          resume_epoch: session.resumeEpoch,
        });
        return;
      }
      case "pause": {
        if (task.phase !== "streaming") {
          sendEvent(task, "error", {
            code: "invalid_request",
            message: "pause is only valid while streaming",
            retryable: false,
            details: null,
          });
          return;
        }
        task.phase = "paused";
        sendEvent(task, "paused", { last_audio_seq: session.lastAudioSeq });
        return;
      }
      case "resume": {
        if (task.phase !== "paused") {
          sendEvent(task, "error", {
            code: "invalid_request",
            message: "resume is only valid while paused",
            retryable: false,
            details: null,
          });
          return;
        }
        task.phase = "streaming";
        return;
      }
      case "stop": {
        if (task.phase !== "streaming") {
          sendEvent(task, "error", {
            code: "invalid_request",
            message: "stop is only valid while streaming",
            retryable: false,
            details: null,
          });
          return;
        }
        task.phase = "finalizing";
        finalizeTask(task);
        return;
      }
      case "cancel": {
        task.phase = "terminal";
        session.state = "cancelled";
        // Results are suppressed: no final event, the close is 1000 (§11.5).
        try {
          task.socket.close(1000, "cancelled");
        } catch {
          /* already closed */
        }
        return;
      }
      default: {
        sendEvent(task, "error", {
          code: "unknown_message_type",
          message: `unsupported client type ${type}`,
          retryable: true,
          details: null,
        });
        return;
      }
    }
  }

  function acceptAudio(task: StreamTask, data: ArrayBuffer | Uint8Array): void {
    const session = sessions.get(task.sessionId);
    if (!session) return;
    if (task.phase !== "streaming") {
      failTask(task, "invalid_request", "audio frames are accepted only while streaming", false, 1008);
      return;
    }
    const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
    if (bytes.length < 12) {
      failTask(task, "invalid_request", "audio frame shorter than the 12-byte header", false, 1008);
      return;
    }
    if (bytes[0] !== 0x56 || bytes[1] !== 0x58) {
      failTask(task, "invalid_request", "audio frame missing the 'VX' magic", false, 1008);
      return;
    }
    if (bytes[2] !== 1) {
      failTask(task, "invalid_request", "unsupported audio frame version", false, 1008);
      return;
    }
    if (bytes[3] !== 1) {
      failTask(task, "invalid_request", "unsupported audio codec", false, 1008);
      return;
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const seq = view.getUint32(4, false);
    const durationMs = view.getUint16(8, false);
    const flags = view.getUint16(10, false);
    if (![10, 20, 30, 40, 60].includes(durationMs)) {
      failTask(task, "invalid_request", "unsupported frame duration", false, 1008);
      return;
    }
    if ((flags & ~1) !== 0) {
      failTask(task, "invalid_request", "reserved flag bits set", false, 1008);
      return;
    }
    const expected = Math.round((SAMPLE_RATE_HZ * durationMs) / 1000);
    const samples = Math.floor((bytes.length - 12) / 2);
    const truncated = bytes.length % 2 !== 0;
    if (truncated || samples !== expected) {
      failTask(task, "invalid_request", `payload length does not match duration: expected ${expected} samples, got ${truncated ? "odd-length" : samples}`, false, 1008);
      return;
    }
    // Duplicate sequences are acknowledged and ignored (§11.4) — but the
    // first frame (seq 0) has no predecessor: `last_audio_seq` starts at 0
    // meaning "none accepted yet", guarded by this flag.
    if (session.sawAudio && seq <= session.lastAudioSeq) {
      sendEvent(task, "audio_ack", { through_seq: seq, received_ms: session.audioReceivedMs, buffered_ms: 0 });
      return;
    }
    if (!session.sawAudio && seq !== 0 && (flags & 1) === 0) {
      failTask(task, "audio_gap", `audio sequence gap at ${seq}`, false, 4004);
      return;
    }
    // A gap closes 4004 unless the discontinuity flag covers it.
    if (session.sawAudio && seq !== session.lastAudioSeq + 1 && (flags & 1) === 0) {
      failTask(task, "audio_gap", `audio sequence gap at ${seq}`, false, 4004);
      return;
    }
    session.sawAudio = true;
    task.framesSeen += 1;
    session.lastAudioSeq = seq;
    session.audioReceivedMs += durationMs;
    sendEvent(task, "audio_ack", { through_seq: seq, received_ms: session.audioReceivedMs, buffered_ms: 0 });
    if (task.framesSeen % 5 === 0) {
      task.partialRevision += 1;
      const transcript = options.transcript ?? "fixture";
      sendEvent(task, "partial", {
        revision: task.partialRevision,
        committed: task.framesSeen >= 10 ? transcript : "",
        tentative: transcript,
        audio_received_ms: session.audioReceivedMs,
        audio_committed_ms: 0,
        stability: null,
        native: true,
      });
    }
    if (task.framesSeen % 12 === 6) {
      sendEvent(task, "speech_start", { audio_ms: session.audioReceivedMs });
    }
  }

  function finalizeTask(task: StreamTask): void {
    const session = sessions.get(task.sessionId);
    if (!session) return;
    const transcript = `${options.transcript ?? "fixture"}-final-${task.framesSeen}`;
    const finalResult = {
      text: transcript,
      segments: [],
      language: "en",
      duration_ms: session.audioReceivedMs,
      model_id: session.modelId ?? selectedModelId,
      history_id: null,
      metrics: { decode_ms: 42, realtime_factor: "0.241" },
    };
    history.push({
      id: history.length + 1,
      timestamp_ms: nowMs(),
      title: "harness",
      transcription_text: transcript,
      duration_ms: session.audioReceivedMs,
      model_id: finalResult.model_id,
      language: finalResult.language,
      source: "stream",
    });
    session.final = finalResult;
    session.state = "completed";
    sendEvent(task, "final", finalResult);
    task.phase = "terminal";
    try {
      task.socket.close(1000, "complete");
    } catch {
      /* already closed */
    }
  }

  /* ------------------------------------------------------------ */
  /* REST                                                          */
  /* ------------------------------------------------------------ */

  function createdResponse(state: SessionState) {
    return {
      session_id: state.id,
      session_token: state.token,
      state: "created",
      ws_url: `/v1/sessions/${state.id}/stream`,
      expires_at: new Date(state.createdAt + 600_000).toISOString(),
      negotiated_audio: { codec: "pcm_s16le", sample_rate_hz: 16_000, channels: 1, chunk_ms: 20 },
      streaming_mode: "native",
      resume_window_ms: 30_000,
    };
  }

  function modelView(model: FakeModel, installedOverride?: boolean) {
    const installed = installedOverride ?? model.installed !== false;
    return {
      id: model.id,
      name: model.name ?? model.id,
      description: "harness model",
      filename: `${model.id}.bin`,
      source_kind: "local",
      size_mb: 75,
      is_downloaded: installed,
      is_downloading: inflightDownloads.has(model.id),
      partial_size: inflightDownloads.has(model.id) ? 10_000_000 : 0,
      is_directory: false,
      engine_type: "transcribe_cpp",
      accuracy_score: 0.5,
      speed_score: 0.9,
      supports_translation: false,
      is_recommended: model.id === "tiny-16k",
      supported_languages: ["en"],
      supports_language_selection: true,
      is_custom: false,
      supports_streaming: true,
      supports_language_detection: true,
      installed,
      download: installed
        ? {
            state: "completed",
            downloaded_bytes: 75_000_000,
            total_bytes: 75_000_000,
            percentage: 100,
          }
        : inflightDownloads.has(model.id)
          ? { state: "downloading", downloaded_bytes: 10_000_000, total_bytes: 75_000_000, percentage: 13 }
          : { state: "not_started", downloaded_bytes: 0, total_bytes: 75_000_000, percentage: 0 },
    };
  }

  async function handleRest(request: Request, url: URL, bearerOk: boolean, ticketOk: boolean, ticketState: TicketState | undefined): Promise<Response | undefined> {
    const path = url.pathname;
    const method = request.method.toUpperCase();

    /* -- unauthenticated --------------------------------------- */
    if (path === "/alive" && method === "GET") {
      return json(200, { alive: true });
    }
    if (!bearerOk && !ticketOk) {
      // 401 with an EMPTY body (no probe enrichment).
      return new Response("", { status: 401 });
    }

    /* -- health/status/config/auth ------------------------------ */
    if (path === "/v1/health" && method === "GET") {
      return json(200, {
        status: "ok",
        version: "0.1.0",
        engine_version: "0.1.0+local",
        protocol_version: 1,
        uptime_ms: nowMs() % 100_000,
        ready: true,
      });
    }
    if (path === "/v1/status" && method === "GET") {
      return json(200, {
        mode: "serve",
        bind: "127.0.0.1:47832",
        active_model_id: selectedModelId,
        model_state: "loaded",
        backend: "transcribe_cpp",
        active_session_id: sessionOrder.find((id) => taskBysession.get(id)?.phase === "streaming") ?? null,
        active_sessions: sessionOrder.filter((id) => sessions.get(id)?.state === "created").length,
        max_realtime_factor: 0.241,
        lease_held: false,
        capabilities: {
          requires_server_vad: false,
          prefers_auto_gain: false,
          prefers_noise_reduction: true,
          max_concurrent_streams: 1,
        },
      });
    }
    if (path === "/v1/config") {
      if (method === "GET") {
        return json(200, {
          bind: "127.0.0.1:47832",
          allowed_origins: [],
          max_sessions: 4,
          max_audio_seconds: 120,
          session_resume_window_ms: 30_000,
          pause_max_ms: 0,
          save_history_default: true,
          vad_enabled_default: true,
          opus_enabled: false,
          restart_required: false,
        });
      }
      if (method === "PATCH") {
        const body = (await request.json().catch(() => ({}))) as Record<string, unknown>;
        if (Object.keys(body).some((k) => !["allowed_origins","max_sessions","max_audio_seconds","session_resume_window_ms","pause_max_ms","save_history_default","vad_enabled_default"].includes(k))) {
          return json(422, envelope("invalid_request", "unknown field", false));
        }
        return json(200, {
          bind: "127.0.0.1:47832",
          allowed_origins: Array.isArray(body.allowed_origins) ? body.allowed_origins : [],
          max_sessions: typeof body.max_sessions === "number" ? body.max_sessions : 4,
          max_audio_seconds: typeof body.max_audio_seconds === "number" ? body.max_audio_seconds : 120,
          session_resume_window_ms: typeof body.session_resume_window_ms === "number" ? body.session_resume_window_ms : 30_000,
          pause_max_ms: typeof body.pause_max_ms === "number" ? body.pause_max_ms : 0,
          save_history_default: body.save_history_default === undefined ? true : body.save_history_default === true,
          vad_enabled_default: body.vad_enabled_default === undefined ? true : body.vad_enabled_default === true,
          opus_enabled: false,
          restart_required: false,
        });
      }
    }
    if (path === "/v1/auth/ws-ticket" && method === "POST") {
      const body = (await request.json().catch(() => ({}))) as { session_id?: string };
      const ticket = randomToken("vx_ticket_");
      tickets.set(ticket, { session: body.session_id, expiresAtMs: nowMs() + 60_000, consumed: false });
      return json(201, { ticket, expires_at: new Date(nowMs() + 60_000).toISOString() });
    }

    /* -- sessions ------------------------------------------------ */
    if (path === "/v1/sessions" && method === "POST") {
      const body = (await request.json().catch(() => ({}))) as Record<string, unknown>;
      if (body.user_defined_extra_field !== undefined) {
        return json(422, envelope("invalid_request", "unknown field", false));
      }
      const key = typeof body.client_session_key === "string" ? body.client_session_key : undefined;
      if (key) {
        for (const id of sessionOrder) {
          const existing = sessions.get(id);
          if (existing && existing.clientKey === key && existing.state === "created") {
            return json(201, createdResponse(existing));
          }
        }
      }
      const id = `s_${harnessId}_${sessions.size + 1}`;
      const state: SessionState = {
        id,
        token: randomToken("vx_sess_"),
        state: "created",
        createdAt: nowMs(),
        clientKey: key,
        modelId: typeof body.model_id === "string" ? body.model_id : undefined,
        audioReceivedMs: 0,
        lastAudioSeq: 0,
        lastEventSeq: 0,
        sawAudio: false,
      };
      sessions.set(id, state);
      sessionOrder.push(id);
      return json(201, createdResponse(state));
    }
    const sessionMatch = /^\/v1\/sessions\/([^/]+)$/.exec(path);
    if (sessionMatch) {
      const id = sessionMatch[1];
      const session = sessions.get(id);
      const subToken = request.headers.get("x-session-token");
      if (!session || (subToken !== session.token && !(ticketOk && ticketState && (ticketState.session ?? id) === id))) {
        // Unknown id and wrong/missing sub-credential are identical (404,
        // never 403 — ids cannot be probed).
        return json(404, envelope("session_not_found", "No such session", false));
      }
      if (method === "GET") {
        return json(200, {
          session_id: session.id,
          state: session.state,
          model_id: session.modelId ?? null,
          language: "auto",
          audio_received_ms: session.audioReceivedMs,
          last_audio_seq: session.lastAudioSeq,
          last_event_seq: session.lastEventSeq,
          created_at: new Date(session.createdAt).toISOString(),
          ...(session.final !== undefined ? { final_result: session.final } : {}),
          ...(session.error !== undefined ? { error: session.error } : {}),
        });
      }
      if (method === "DELETE") {
        session.state = "cancelled";
        session.final = undefined;
        const task = taskBysession.get(id);
        if (task && task.phase !== "terminal") {
          task.phase = "terminal";
          try {
            task.socket.close(1000, "cancelled");
          } catch {
            /* socket gone */
          }
        }
        return new Response("", { status: 204 });
      }
    }

    /* -- models -------------------------------------------------- */
    if (path === "/v1/models" && method === "GET") {
      return json(200, {
        models: models.map((m) => modelView(m)),
        selected_model_id: selectedModelId,
        loaded_model_id: loadedModelId,
      });
    }
    const downloadsMatch = /^\/v1\/models\/(.+)\/downloads$/.exec(path);
    if (downloadsMatch && method === "GET") {
      const id = decodeURIComponent(downloadsMatch[1]);
      const model = models.find((m) => m.id === id);
      if (!model) return json(404, envelope("model_not_found", "No such model", false));
      if (inflightDownloads.has(id)) {
        return json(200, {
          state: "downloading",
          downloaded_bytes: 10_000_000,
          total_bytes: 75_000_000,
          percentage: 13,
        });
      }
      return json(200, modelView(model).download);
    }
    const downloadMatch = /^\/v1\/models\/(.+)\/download$/.exec(path);
    if (downloadMatch) {
      const id = decodeURIComponent(downloadMatch[1]);
      const model = models.find((m) => m.id === id);
      if (!model) return json(404, envelope("model_not_found", "No such model", false));
      const installed = model.installed !== false && !inflightDownloads.has(id);
      if (method === "POST") {
        // Already installed → 200; otherwise starts (or converges on) the
        // single in-flight download → 202 with the status projection.
        if (installed) {
          return json(200, {
            state: "completed",
            downloaded_bytes: 75_000_000,
            total_bytes: 75_000_000,
            percentage: 100,
          });
        }
        inflightDownloads.add(id);
        return json(202, {
          state: "downloading",
          downloaded_bytes: 10_000_000,
          total_bytes: 75_000_000,
          percentage: 13,
        });
      }
      if (method === "DELETE") {
        if (!inflightDownloads.has(id)) {
          return json(422, envelope("invalid_request", `nothing_to_cancel: no download in flight for model '${id}'`, false));
        }
        inflightDownloads.delete(id);
        return new Response("", { status: 204 });
      }
    }
    const modelMatch = /^\/v1\/models\/([^/]+)$/.exec(path);
    if (modelMatch && method === "GET") {
      const id = decodeURIComponent(modelMatch[1]);
      const model = models.find((m) => m.id === id);
      if (!model) return json(404, envelope("model_not_found", "No such model", false));
      return json(200, { model: modelView(model), prepare: { stage: "ok" } });
    }
    if (path === "/v1/models/selected" && method === "PUT") {
      const body = (await request.json().catch(() => ({}))) as { model_id?: string; load?: string };
      const model = models.find((m) => m.id === body.model_id);
      if (!model) return json(404, envelope("model_not_found", "No such model", false));
      selectedModelId = model.id;
      loadedModelId = body.load === "lazy" ? loadedModelId : model.id;
      return json(200, {
        model_id: model.id,
        load: body.load === "lazy" ? "lazy" : "eager",
        loaded: body.load !== "lazy",
        model: modelView(model),
      });
    }

    /* -- prepare SSE --------------------------------------------- */
    const prepareMatch = /^\/v1\/models\/(.+)\/prepare$/.exec(path);
    if (prepareMatch && method === "POST") {
      if (prepareSlot.held) {
        return json(409, envelope("prepare-busy", "another model prepare is already in flight", true), { "Retry-After": "2" });
      }
      prepareSlot.held = true;
      const release = () => {
        prepareSlot.held = false;
      };
      const id = decodeURIComponent(prepareMatch[1]);
      const model = models.find((m) => m.id === id);
      if (!model) {
        release();
        return json(404, envelope("model_not_found", "No such model", false));
      }
      const chunks: string[] = [];
      const emit = (event: string, payload: unknown) => {
        chunks.push(`event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`);
      };
      emit("verifying", { model_id: id, stage: "verifying" });
      emit("loading", { model_id: id, stage: "loading" });
      if (model.prepareError) {
        emit("error", { code: model.prepareError.code, message: model.prepareError.message, retryable: model.prepareError.retryable });
      } else {
        emit("ok", { model_id: id, backend: "transcribe_cpp", stage: "ok" });
      }
      let index = 0;
      const stream = new ReadableStream<Uint8Array>({
        pull(controller) {
          if (index < chunks.length) {
            controller.enqueue(new TextEncoder().encode(chunks[index++]));
            return Promise.resolve();
          }
          release();
          controller.close();
          return Promise.resolve();
        },
        cancel() {
          release();
        },
      });
      return new Response(stream, {
        status: 200,
        headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" },
      });
    }

    /* -- batch ---------------------------------------------------- */
    if (path === "/v1/transcriptions" && method === "POST") {
      const bytes = new Uint8Array(await request.arrayBuffer());
      if (bytes.length < 64) {
        return json(400, envelope("invalid-audio", "WAV validation failed: file too short", false));
      }
      const jobId = `job_${jobs.size + 1}`;
      jobs.set(jobId, { state: "processing" });
      setTimeout(() => {
        const job = jobs.get(jobId);
        if (job && job.state === "processing") {
          job.state = "completed";
          job.result = { transcript: "batch-fixture-transcript", duration_ms: 400 };
          history.push({
            id: history.length + 1,
            timestamp_ms: nowMs(),
            title: "harness",
            transcription_text: "batch-fixture-transcript",
            duration_ms: 400,
            source: "batch",
          });
        }
      }, 10);
      return json(202, { job_id: jobId, state: "processing", status_url: `/v1/transcriptions/${jobId}` });
    }
    const jobMatch = /^\/v1\/transcriptions\/([^/]+)$/.exec(path);
    if (jobMatch) {
      const job = jobs.get(jobMatch[1]);
      if (!job) return json(404, envelope("not_found", "no such job", false));
      if (method === "GET") {
        return json(200, {
          job_id: jobMatch[1],
          state: job.state,
          ...(job.result ? { result: job.result } : {}),
        });
      }
      if (method === "DELETE") {
        job.state = "cancelled";
        return new Response("", { status: 204 });
      }
    }

    /* -- history --------------------------------------------------- */
    if (path === "/v1/history" && method === "GET") {
      return json(200, { entries: history.slice(0, 20), has_more: false, next_cursor: null });
    }
    const historyMatch = /^\/v1\/history\/(\d+)$/.exec(path);
    if (historyMatch && method === "GET") {
      const entry = history.find((e) => e.id === Number(historyMatch[1]));
      if (!entry) return json(404, envelope("not_found", "no such history entry", false));
      return json(200, entry);
    }

    return json(404, envelope("not_found", "no route", false));
  }

  /* ------------------------------------------------------------ */
  /* Server                                                        */
  /* ------------------------------------------------------------ */

  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    development: false,
    fetch: (request: Request, bunServer: { upgrade: (req: Request, opts?: { data?: unknown }) => boolean }) => {
      const url = new URL(request.url);
      const path = url.pathname;
      const authHeader = request.headers.get("authorization");
      const bearer = authHeader?.startsWith("Bearer ") ? authHeader.slice(7) : undefined;
      const bearerOk = bearer === primaryToken;
      const ticketParam = url.searchParams.get("ticket");
      const isStreamPath = /^\/v1\/sessions\/[^/]+\/stream\/?$/.test(path);
      const ticketState = ticketParam ? tickets.get(ticketParam) : undefined;
      const ticketOk =
        ticketState !== undefined && !ticketState.consumed && ticketState.expiresAtMs > nowMs()
          ? isStreamPath
          : false;

      /* -- WS upgrade path ---------------------------------------- */
      if (isStreamPath) {
        const sessionMatch = /^\/v1\/sessions\/([^/]+)\/stream\/?$/.exec(path);
        const sessionId = sessionMatch?.[1];
        const session = sessionId ? sessions.get(sessionId) : undefined;
        const subToken = request.headers.get("x-session-token");

        // Pre-upgrade refusals arrive as REST-shaped responses (§11.2).
        if (!bearerOk && !ticketOk) {
          return new Response("", { status: 401 });
        }
        if (!sessionId || !session || session.state !== "created") {
          return json(404, envelope("session_not_found", "No such session", false));
        }
        const subOk = subToken === session.token;
        const ticketBound =
          ticketState !== undefined &&
          ticketOk &&
          (ticketState.session ?? sessionId) === sessionId;
        if (!subOk && !ticketBound) {
          return json(404, envelope("session_not_found", "No such session", false));
        }
        const existingTask = taskBysession.get(sessionId);
        if (existingTask && existingTask.phase !== "terminal") {
          return json(503, envelope("service_full", "A stream socket is already attached to this session", true), { "Retry-After": "1" });
        }
        if (ticketBound && ticketParam && ticketState) ticketState.consumed = true;

        const upgraded = bunServer.upgrade(request, { data: { sessionId } });
        if (!upgraded) {
          return json(503, envelope("service_full", "upgrade declined", true), { "Retry-After": "1" });
        }
        return undefined; // handshake took over
      }

      const response = handleRest(request, url, bearerOk, ticketOk, ticketState);
      return response !== undefined
        ? Promise.resolve(response)
        : json(404, envelope("not_found", "no route", false));
    },
    websocket: {
      // Bun wires events after `server.upgrade` with ws.data carrying session.
      open(socket: any) {
        const sessionId = (socket.data as { sessionId?: string }).sessionId;
        const session = sessionId ? sessions.get(sessionId) : undefined;
        if (!session || sessionId === undefined) {
          socket.close(1008, "session_not_found");
          return;
        }
        const task: StreamTask = {
          sessionId,
          phase: "awaiting_start",
          seq: 1,
          framesSeen: 0,
          partialRevision: 0,
          socket,
        };
        taskBysession.set(sessionId, task);
        sendEvent(task, "hello", {
          protocol_version: 1,
          heartbeat_ms: 15_000,
          max_frame_bytes: 65_536,
          resume_window_ms: 30_000,
          negotiated_audio: { codec: "pcm_s16le", sample_rate_hz: 16_000, channels: 1, chunk_ms: 20 },
          streaming_mode: "native",
        });
      },
      message(socket: any, raw: string | Uint8Array | ArrayBuffer) {
        const sessionId = (socket.data as { sessionId?: string }).sessionId;
        const task = sessionId ? taskBysession.get(sessionId) : undefined;
        if (!task) return;
        if (typeof raw !== "string") {
          acceptAudio(task, raw as ArrayBuffer | Uint8Array);
          return;
        }
        let parsed: unknown;
        try {
          parsed = JSON.parse(raw);
        } catch {
          failTask(task, "invalid_request", "control frame is not JSON", false, 1008);
          return;
        }
        const record = parsed as { type?: string; payload?: Record<string, unknown> };
        if (typeof record.type !== "string") {
          failTask(task, "invalid_request", "control frame lacks a type", false, 1008);
          return;
        }
        dispatchControl(task, record.type, record.payload ?? {});
      },
      close(socket: any) {
        const sessionId = (socket.data as { sessionId?: string }).sessionId;
        if (!sessionId) return;
        const task = taskBysession.get(sessionId);
        if (!task) return;
        taskBysession.delete(sessionId);
        const session = sessions.get(sessionId);
        if (session && task.phase !== "terminal") {
          if (options.keepAliveOnClose === true) return;
          // Socket death cancels the session (M2 §7 teardown).
          session.state = "cancelled";
          task.phase = "terminal";
        }
      },
    },
  });

  const harness: Harness = {
    get baseUrl() {
      return `http://127.0.0.1:${server.port}`;
    },
    get token() {
      return primaryToken;
    },
    get port() {
      return server.port;
    },
    async stop() {
      server.stop(true);
    },
    sessionState(id: string): SessionSnapshot | undefined {
      const s = sessions.get(id);
      if (!s) return undefined;
      return {
        id: s.id,
        state: s.state,
        audioReceivedMs: s.audioReceivedMs,
        lastAudioSeq: s.lastAudioSeq,
        lastEventSeq: s.lastEventSeq,
        final: s.final,
        error: s.error,
      };
    },
    allSessionStates(): SessionSnapshot[] {
      return sessionOrder.map((id) => sessions.get(id)).filter((s): s is SessionState => s !== undefined).map((s) => ({
        id: s.id,
        state: s.state,
        audioReceivedMs: s.audioReceivedMs,
        lastAudioSeq: s.lastAudioSeq,
        lastEventSeq: s.lastEventSeq,
        final: s.final,
        error: s.error,
      }));
    },
    cancelSession(id: string): void {
      const s = sessions.get(id);
      if (s) {
        s.state = "cancelled";
        const task = taskBysession.get(id);
        if (task && task.phase !== "terminal") {
          task.phase = "terminal";
          try {
            task.socket.close(1000, "cancelled");
          } catch {
            /* socket gone */
          }
        }
      }
    },
    holdPrepare(): () => void {
      prepareSlot.held = true;
      return () => {
        prepareSlot.held = false;
      };
    },
  };

  return Promise.resolve(harness);
}
