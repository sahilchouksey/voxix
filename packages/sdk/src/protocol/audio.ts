/**
 * The §6 binary audio frame codec, byte-exact with the service
 * (`core/src/service/audio.rs`):
 *
 *   byte 0..1   ASCII "VX"
 *   byte 2      version 1
 *   byte 3      codec (1 = pcm_s16le, 2 = opus)
 *   byte 4..7   big-endian u32 audio sequence (starts at 0)
 *   byte 8..9   big-endian u16 frame duration (10/20/30/40/60 ms)
 *   byte 10..11 big-endian u16 flags (bit 0 = discontinuity; others zero)
 *   rest        little-endian signed mono PCM16 samples (16 kHz)
 *
 * Payload length must equal 16 kHz * duration_ms / 1000 samples.
 */

export const AUDIO_HEADER_BYTES = 12;
export const CODEC_PCM_S16LE = 1;
export const CODEC_OPUS = 2;
export const FLAG_DISCONTINUITY = 1;
export const SAMPLE_RATE_HZ = 16_000;
export const LEGAL_FRAME_DURATIONS = [10, 20, 30, 40, 60] as const;
export const OPUS_MAX_PACKET_BYTES = 1276;
export const OPUS_MAX_PACKET_FRAMES = 48;

export type FrameDurationMs = 10 | 20 | 30 | 40 | 60;

/** One parsed or with-error codec result. */
export interface AudioFrame {
  seq: number;
  duration_ms: number;
  discontinuity: boolean;
  pcm: Int16Array;
}

export type FrameErrorReason =
  | "short"
  | "magic"
  | "version"
  | "codec"
  | "duration"
  | "flags"
  | "payload_length";

export interface FrameError {
  reason: FrameErrorReason;
  message: string;
}

export function samplesFor(durationMs: number): number {
  return Math.round((SAMPLE_RATE_HZ * durationMs) / 1000);
}

/**
 * Serialize a frame. `pcm` length must match the duration exactly.
 * Throws ProtocolError on any violation (client-side discipline mirrors the
 * server's `invalid_request` frame rejections).
 */
export function encodeAudioFrame(
  pcm: Int16Array | Uint8Array,
  durationMs: FrameDurationMs,
  seq: number,
  discontinuity = false,
): Uint8Array {
  if (!LEGAL_FRAME_DURATIONS.includes(durationMs)) {
    throw new Error(`unsupported frame duration ${durationMs} (10/20/30/40/60 ms)`);
  }
  const expected = samplesFor(durationMs);
  const payload =
    pcm instanceof Int16Array
      ? pcm
      : i16FromBytes(pcm, durationMs);
  if (payload.length !== expected) {
    throw new Error(
      `payload length does not match duration: expected ${expected} samples, got ${payload.length}`,
    );
  }
  const out = new Uint8Array(AUDIO_HEADER_BYTES + payload.length * 2);
  out[0] = 0x56; // 'V'
  out[1] = 0x58; // 'X'
  out[2] = 1;
  out[3] = CODEC_PCM_S16LE;
  view32(out, 4).setUint32(0, seq >>> 0, false); // big-endian
  view16(out, 8).setUint16(0, durationMs, false);
  view16(out, 10).setUint16(0, discontinuity ? FLAG_DISCONTINUITY : 0, false);
  const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);
  for (let i = 0; i < payload.length; i++) {
    dv.setInt16(AUDIO_HEADER_BYTES + i * 2, payload[i], true); // little-endian
  }
  return out;
}

/**
 * Frame a raw Opus packet (RFC 6716: 1..=1276 bytes) into a codec-2 binary
 * frame. Client framing only: producing the packet itself is the native
 * encoder's job; this maps a ready packet onto the §6 header.
 */
export function encodeOpusFrame(
  packet: Uint8Array,
  durationMs: FrameDurationMs,
  seq: number,
  discontinuity = false,
): Uint8Array {
  if (packet.length < 1 || packet.length > OPUS_MAX_PACKET_BYTES) {
    throw new Error(
      `opus packet length must be 1..${OPUS_MAX_PACKET_BYTES} bytes, got ${packet.length}`,
    );
  }
  const out = new Uint8Array(AUDIO_HEADER_BYTES + packet.length);
  out[0] = 0x56; // 'V'
  out[1] = 0x58; // 'X'
  out[2] = 1;
  out[3] = CODEC_OPUS;
  view32(out, 4).setUint32(0, seq >>> 0, false); // big-endian
  view16(out, 8).setUint16(0, durationMs, false);
  view16(out, 10).setUint16(0, discontinuity ? FLAG_DISCONTINUITY : 0, false);
  out.set(packet, AUDIO_HEADER_BYTES);
  return out;
}

/** Validate raw i16 LE bytes against the declared duration, LE everywhere. */
function i16FromBytes(bytes: Uint8Array, durationMs: FrameDurationMs): Int16Array {
  const expected = samplesFor(durationMs);
  if (bytes.length % 2 !== 0 || bytes.length / 2 !== expected) {
    throw new Error(
      `payload length does not match duration: expected ${expected} samples, got ${bytes.length / 2}`,
    );
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pcm = new Int16Array(expected);
  for (let i = 0; i < expected; i++) {
    pcm[i] = dv.getInt16(i * 2, true);
  }
  return pcm;
}

function view32(bytes: Uint8Array, offset: number): DataView {
  return new DataView(bytes.buffer, bytes.byteOffset + offset, 4);
}

function view16(bytes: Uint8Array, offset: number): DataView {
  return new DataView(bytes.buffer, bytes.byteOffset + offset, 2);
}

/**
 * Parse a wire frame (mainly for interop tests and non-SDK readers).
 * Returns the frame or a structured rejection.
 */
export function parseAudioFrame(bytes: Uint8Array): { frame?: AudioFrame; error?: FrameError } {
  if (bytes.length < AUDIO_HEADER_BYTES) {
    return { error: { reason: "short", message: "audio frame shorter than the 12-byte header" } };
  }
  if (bytes[0] !== 0x56 || bytes[1] !== 0x58) {
    return { error: { reason: "magic", message: "audio frame missing the 'VX' magic" } };
  }
  if (bytes[2] !== 1) {
    return { error: { reason: "version", message: "unsupported audio frame version (only 1)" } };
  }
  if (bytes[3] !== CODEC_PCM_S16LE) {
    return { error: { reason: "codec", message: "unsupported audio codec (only pcm_s16le in v1)" } };
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const seq = dv.getUint32(4, false);
  const durationMs = dv.getUint16(8, false);
  const flags = dv.getUint16(10, false);
  if (!(LEGAL_FRAME_DURATIONS as readonly number[]).includes(durationMs)) {
    return { error: { reason: "duration", message: "unsupported frame duration (10/20/30/40/60 ms)" } };
  }
  if ((flags & ~FLAG_DISCONTINUITY) !== 0) {
    return { error: { reason: "flags", message: "reserved flag bits set" } };
  }
  const payload = bytes.subarray(AUDIO_HEADER_BYTES);
  const expected = samplesFor(durationMs);
  if (payload.length % 2 !== 0 || payload.length / 2 !== expected) {
    return {
      error: {
        reason: "payload_length",
        message: `payload length does not match duration: expected ${expected} samples, got ${(payload.length / 2) | 0}`,
      },
    };
  }
  const pcm = new Int16Array(expected);
  for (let i = 0; i < expected; i++) {
    pcm[i] = dv.getInt16(AUDIO_HEADER_BYTES + i * 2, true); // little-endian
  }
  return {
    frame: { seq, duration_ms: durationMs, discontinuity: (flags & FLAG_DISCONTINUITY) !== 0, pcm },
  };
}
