/**
 * Protocol v1 wire types, re-declared from the as-implemented service (the
 * Rust DTOs are protocol truth; these mirror them — no server types are
 * imported). Field names are the wire's snake_case verbatim: the SDK does no
 * case transformation, and unknown payload keys are ignored (wire-evolution
 * rule: additive only).
 */

/* ------------------------------------------------------------------ */
/* Envelope and payload vocabulary                                     */
/* ------------------------------------------------------------------ */

/** `{v:1,type,session_id,seq,ts_ms,payload}` — `seq` monotone from 1. */
export interface Envelope<P = unknown> {
  v: number;
  type: string;
  session_id: string;
  seq: number;
  ts_ms: number;
  payload: P;
}

export interface NegotiatedAudio {
  codec: string;
  sample_rate_hz: number;
  channels: number;
  chunk_ms: number;
}

export interface HelloPayload {
  protocol_version: number;
  heartbeat_ms: number;
  max_frame_bytes: number;
  resume_window_ms: number;
  negotiated_audio: NegotiatedAudio;
  streaming_mode: string;
}

export interface StartedPayload {
  model_id: string;
  backend: string | null;
  resumed: boolean;
  next_audio_seq: number;
  /** Barrier-attached resume pair (M3 fencing). */
  resume_token: string;
  resume_epoch: number;
}

export interface PausedPayload {
  last_audio_seq: number;
}

export interface SpeechPayload {
  audio_ms: number;
}

export interface AudioAckPayload {
  through_seq: number;
  received_ms: number;
  buffered_ms: number;
}

export interface PartialPayload {
  revision: number;
  committed: string;
  tentative: string;
  audio_received_ms: number;
  audio_committed_ms: number;
  stability: number | null;
  native: boolean;
}

export interface FinalizingPayload {
  source: string;
}

export interface FinalMetrics {
  decode_ms: number;
  realtime_factor?: string | number;
}

export interface Segment {
  /** Additive-safe: the v1 service emits `segments: []`; future shapes are
   * documented here, not fabricated. */
  start?: number;
  end?: number;
  text?: string;
}

export interface FinalResult {
  text: string;
  segments: Segment[];
  language: string;
  duration_ms: number;
  model_id: string;
  history_id: string | number | null;
  metrics: FinalMetrics;
}

export interface ErrorPayload {
  code: string;
  message: string;
  retryable: boolean;
  details?: unknown;
}

export interface PongPayload {
  nonce: number;
}

/* ------------------------------------------------------------------ */
/* SessionEvent (the tap vocabulary)                                   */
/* ------------------------------------------------------------------ */

/** One observation event on a session tap. `resumed` is client-synthesized on
 * `resume()`; every other type is a server wire event. */
export type SessionEvent =
  | { type: "hello"; payload: HelloPayload }
  | { type: "started"; payload: StartedPayload }
  | { type: "paused"; payload: PausedPayload }
  /** Client-synthesized; the wire carries no `resumed` event in v1. */
  | { type: "resumed"; payload: { last_audio_seq: number } }
  | { type: "speech_start"; payload: SpeechPayload }
  | { type: "speech_end"; payload: SpeechPayload }
  | { type: "audio_ack"; payload: AudioAckPayload }
  | { type: "partial"; payload: PartialPayload }
  | { type: "finalizing"; payload: FinalizingPayload }
  | { type: "final"; payload: FinalResult }
  | { type: "error"; payload: ErrorPayload }
  | { type: "pong"; payload: PongPayload };

/* ------------------------------------------------------------------ */
/* REST DTOs (as implemented; additive)                                */
/* ------------------------------------------------------------------ */

export interface Health {
  status: string;
  version: string;
  engine_version: string;
  protocol_version: number;
  uptime_ms: number;
  ready: boolean;
}

export interface Capabilities {
  requires_server_vad: boolean;
  prefers_auto_gain: boolean;
  prefers_noise_reduction: boolean;
  max_concurrent_streams: number;
}

export interface ServerStatus {
  mode: string;
  bind: string;
  active_model_id: string | null;
  model_state: string;
  backend: string | null;
  active_session_id: string | null;
  active_sessions: number;
  max_realtime_factor: number | null;
  lease_held: boolean;
  capabilities: Capabilities;
}

export interface ServiceConfig {
  bind: string;
  allowed_origins: string[];
  max_sessions: number;
  max_audio_seconds: number;
  session_resume_window_ms: number;
  pause_max_ms: number;
  save_history_default: boolean;
  vad_enabled_default: boolean;
  opus_enabled: boolean;
  restart_required: boolean;
}

/** PATCH /v1/config request — mutable set only; unknown fields are 422. */
export interface ServiceConfigPatch {
  allowed_origins?: string[];
  max_sessions?: number;
  max_audio_seconds?: number;
  session_resume_window_ms?: number;
  pause_max_ms?: number;
  save_history_default?: boolean;
  vad_enabled_default?: boolean;
  opus_enabled?: boolean;
}

/* -- sessions -------------------------------------------------------- */

export interface CreateSessionAudio {
  codec?: string;
  sample_rate_hz?: number;
  channels?: number;
  chunk_ms?: number;
}

export interface CreateSessionVad {
  mode?: "server" | "client" | "disabled";
  auto_finalize_ms?: number;
}

/**
 * POST /v1/sessions body. Wire-shaped (`snake_case`); the canonical SDK
 * names match the wire so requests stay inspectable. `model` is accepted as
 * an alias for `model_id` (mirrors the service).
 */
export interface CreateSessionBody {
  model_id?: string;
  /** Alias of `model_id` (do not set both). */
  model?: string;
  language?: string;
  task?: string;
  audio?: CreateSessionAudio;
  vad?: CreateSessionVad;
  partial_results?: boolean;
  save_history?: boolean;
  client_session_key?: string;
}

export interface CreatedSession {
  session_id: string;
  session_token: string;
  state: string;
  ws_url: string;
  expires_at: string;
  negotiated_audio: NegotiatedAudio;
  streaming_mode: string;
  resume_window_ms: number;
}

export interface SessionView {
  session_id: string;
  state: string;
  model_id: string | null;
  language: string;
  audio_received_ms: number;
  last_audio_seq: number;
  last_event_seq: number;
  created_at: string;
  final_result?: FinalResult;
  error?: ErrorPayload;
}

/* -- models ---------------------------------------------------------- */

export interface ModelDownloadStatus {
  state: string;
  downloaded_bytes: number;
  total_bytes: number;
  percentage?: number;
  error?: string;
}

export interface ModelView {
  id: string;
  name: string;
  description: string;
  filename: string;
  source_kind: string;
  size_mb: number;
  is_downloaded: boolean;
  is_downloading: boolean;
  partial_size: number;
  is_directory: boolean;
  engine_type: string;
  accuracy_score: number;
  speed_score: number;
  supports_translation: boolean;
  is_recommended: boolean;
  supported_languages: string[];
  supports_language_selection: boolean;
  is_custom: boolean;
  supports_streaming: boolean;
  supports_language_detection: boolean;
  installed: boolean;
  download: ModelDownloadStatus;
}

export interface ModelsPage {
  models: ModelView[];
  selected_model_id: string | null;
  loaded_model_id: string | null;
}

export type PrepareStage =
  | "not_started"
  | "downloading"
  | "verifying"
  | "loading"
  | "ok"
  | "failed";

export interface SingleModel {
  model: ModelView;
  prepare: { stage: PrepareStage };
}

export type ModelSelectionLoad = "eager" | "lazy";

export interface ModelSelection {
  model_id: string;
  load: ModelSelectionLoad;
  loaded: boolean;
  model: ModelView;
}

/** Terminal projection resolved by `prepareModel()` (SSE consumer). */
export interface ModelDownload {
  /** SSE terminal stage mapped onto the download-status vocabulary. */
  state: "loading" | "completed" | "failed";
  downloaded_bytes: number;
  total_bytes: number;
  percentage?: number;
  error?: string;
  backend?: string;
}

/** One prepare SSE event (spec §15 G7 vocabulary, as implemented). `event` is
 * the SSE event name carried verbatim; keep-alive comments never surface. */
export type PrepareEvent =
  | { event: "verifying"; model_id: string }
  | {
      event: "downloading";
      model_id: string;
      downloaded_bytes: number;
      total_bytes: number;
    }
  | { event: "loading"; model_id: string }
  | { event: "ok"; model_id: string; backend: string }
  | { event: "error"; code: string; message: string; retryable: boolean };

/* -- batch ----------------------------------------------------------- */

export interface BatchJob {
  job_id: string;
  state: string;
  status_url: string;
}

export interface BatchJobStatus {
  job_id: string;
  state: "processing" | "completed" | "failed" | "cancelled";
  result?: { transcript: string; duration_ms: number };
  error?: {
    code: string;
    message: string;
    retryable: boolean;
    details?: unknown;
  };
}

/* -- history --------------------------------------------------------- */

export interface HistoryEntry {
  id: number;
  timestamp_ms: number;
  title: string;
  transcription_text: string;
  duration_ms?: number;
  model_id?: string;
  language?: string;
  source: "batch" | "stream" | string;
}

export interface HistoryPage {
  entries: HistoryEntry[];
  has_more: boolean;
  next_cursor?: number | null;
}

/* -- auth ------------------------------------------------------------ */

export interface CreatedToken {
  id: string;
  name: string;
  token: string;
  prefix: string;
  created_at: string;
}

export interface WsTicket {
  ticket: string;
  expires_at: string;
}

/* ------------------------------------------------------------------ */
/* Shared error envelope                                               */
/* ------------------------------------------------------------------ */

export interface VxErrorEnvelopeBody {
  code: string;
  message: string;
  retryable: boolean;
  request_id: string;
  details?: unknown;
}

export interface VxErrorEnvelope {
  error: VxErrorEnvelopeBody;
}
