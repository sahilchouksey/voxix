/**
 * @voxix/sdk — core entry (platform-neutral: no DOM, no Node built-ins).
 * Subpaths: `@voxix/sdk/web` (browser capture helpers, DOM), `@voxix/sdk/node`
 * (file helpers + the embedded-child `create()`, Node/Bun).
 */
export {
  VoxixClient,
  DEFAULT_TIMEOUTS,
  errorFromResponse,
  type VoxixClientOptions,
  type SessionTimeouts,
} from "./client.js";

export {
  VoxixSession,
  closeError,
  type SessionPhase,
  type SessionStartOptions,
  type ResumeCredentials,
  type VoxixSessionConfig,
} from "./session.js";

/** The six tagged error classes (§15 G4). */
export {
  VoxixError,
  TransportError,
  AuthError,
  ProtocolError,
  ServiceError,
  ModelError,
  CancelledError,
  type VoxixErrorCode,
  type VoxixErrorInit,
} from "./errors.js";

/** Bounded drop-oldest single-consumer observation tap (§15 G25). */
export { BoundedTap, STRICT_CONSUMER_MESSAGE } from "./async-queue.js";

/** Transport seams (spec §8: injected fetch + webSocketFactory). */
export type {
  FetchLike,
  FetchResponseLike,
  AbortSignalLike,
  WebSocketFactory,
  WebSocketLike,
} from "./transport.js";

/** Audio frame codec (§6 12-byte header, byte-exact with the service). */
export {
  AUDIO_HEADER_BYTES,
  CODEC_PCM_S16LE,
  CODEC_OPUS,
  FLAG_DISCONTINUITY,
  OPUS_MAX_PACKET_BYTES,
  OPUS_MAX_PACKET_FRAMES,
  SAMPLE_RATE_HZ,
  encodeAudioFrame,
  encodeOpusFrame,
  parseAudioFrame,
  samplesFor,
  type AudioFrame,
  type FrameDurationMs,
  type FrameError,
  type FrameErrorReason,
} from "./protocol/audio.js";

/** Wire types (snake_case verbatim; additive-only evolution). */
export type {
  Envelope,
  HelloPayload,
  StartedPayload,
  PausedPayload,
  SpeechPayload,
  AudioAckPayload,
  PartialPayload,
  FinalizingPayload,
  FinalResult,
  FinalMetrics,
  Segment,
  ErrorPayload,
  PongPayload,
  SessionEvent,
  Health,
  Capabilities,
  ServerStatus,
  ServiceConfig,
  ServiceConfigPatch,
  CreateSessionAudio,
  CreateSessionVad,
  CreateSessionBody,
  CreatedSession,
  SessionView,
  ModelDownloadStatus,
  ModelView,
  ModelsPage,
  PrepareStage,
  SingleModel,
  ModelSelection,
  ModelSelectionLoad,
  ModelDownload,
  PrepareEvent,
  BatchJob,
  BatchJobStatus,
  HistoryEntry,
  HistoryPage,
  CreatedToken,
  WsTicket,
  VxErrorEnvelope,
} from "./protocol/index.js";
