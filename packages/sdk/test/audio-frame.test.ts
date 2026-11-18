import { describe, expect, test } from "bun:test";
import {
  AUDIO_HEADER_BYTES,
  CODEC_OPUS,
  FLAG_DISCONTINUITY,
  OPUS_MAX_PACKET_BYTES,
  SAMPLE_RATE_HZ,
  encodeAudioFrame,
  encodeOpusFrame,
  parseAudioFrame,
  samplesFor,
} from "../src/protocol/audio.js";

describe("§6 binary audio frame codec", () => {
  test("20 ms header is byte-exact: VX, version 1, codec 1, BE seq, BE duration, BE flags, LE payload", () => {
    const samples = samplesFor(20); // 320
    const pcm = new Int16Array(samples);
    for (let i = 0; i < samples; i++) pcm[i] = i * 7;
    const wire = encodeAudioFrame(pcm, 20, 0);
    expect(wire.byteLength).toBe(AUDIO_HEADER_BYTES + samples * 2);
    // ASCII magic + version + codec
    expect(wire[0]).toBe(0x56); // 'V'
    expect(wire[1]).toBe(0x58); // 'X'
    expect(wire[2]).toBe(1);
    expect(wire[3]).toBe(1); // pcm_s16le
    // big-endian sequence u32
    const dv = new DataView(wire.buffer, wire.byteOffset, wire.byteLength);
    expect(dv.getUint32(4, false)).toBe(0);
    expect(dv.getUint16(8, false)).toBe(20);
    expect(dv.getUint16(10, false)).toBe(0);
    // little-endian signed payload — verify a handful of samples
    expect(dv.getInt16(12, true)).toBe(0);
    expect(dv.getInt16(14, true)).toBe(7);
    expect(dv.getInt16(12 + 630, true)).toBe(315 * 7);
  });

  test("parse round-trip preserves seq/duration/flags/pcm", () => {
    const pcm = new Int16Array([1, -2, 3, -4, 5]);
    const padded = new Int16Array(samplesFor(10));
    padded.set(pcm.subarray(0, Math.min(5, padded.length)));
    const wire = encodeAudioFrame(padded, 10, 84123);
    const parsed = parseAudioFrame(wire);
    expect(parsed.error).toBeUndefined();
    const frame = parsed.frame;
    expect(frame?.seq).toBe(84123);
    expect(frame?.duration_ms).toBe(10);
    expect(frame?.discontinuity).toBe(false);
    expect(Array.from(frame?.pcm ?? [])).toEqual(Array.from(padded));
  });

  test("discontinuity flag round-trips (bit 0 only)", () => {
    const wire = encodeAudioFrame(new Int16Array(samplesFor(30)), 30, 77, true);
    const dv = new DataView(wire.buffer, wire.byteOffset, wire.byteLength);
    expect(dv.getUint16(10, false)).toBe(FLAG_DISCONTINUITY);
    const parsed = parseAudioFrame(wire);
    expect(parsed.frame?.discontinuity).toBe(true);
  });

  test("payload-length mismatch is rejected", () => {
    const short = encodeAudioFrame(new Int16Array(samplesFor(10)), 10, 1).subarray(0, 12 + 100);
    expect(parseAudioFrame(new Uint8Array(short)).error?.reason).toBe("payload_length");
    // odd byte count
    const odd = encodeAudioFrame(new Int16Array(samplesFor(10)), 10, 1);
    const oddBytes = new Uint8Array([...Array.from(odd.subarray(0, 12 + 10)), 0]);
    expect(parseAudioFrame(oddBytes).error?.reason).toBe("payload_length");
  });

  test("illegal durations/flags/magic are rejected", () => {
    expect(() => encodeAudioFrame(new Int16Array(160), 15 as never, 0)).toThrow(/duration/);
    const badMagic = encodeAudioFrame(new Int16Array(samplesFor(10)), 10, 0);
    badMagic[1] = 0x59;
    expect(parseAudioFrame(badMagic).error?.reason).toBe("magic");
    const badVersion = encodeAudioFrame(new Int16Array(samplesFor(10)), 10, 0);
    badVersion[2] = 2;
    expect(parseAudioFrame(badVersion).error?.reason).toBe("version");
    const badCodec = encodeAudioFrame(new Int16Array(samplesFor(10)), 10, 0);
    badCodec[3] = 9;
    expect(parseAudioFrame(badCodec).error?.reason).toBe("codec");
    const badFlags = encodeAudioFrame(new Int16Array(samplesFor(10)), 10, 0);
    new DataView(badFlags.buffer).setUint16(10, 0b10, false);
    expect(parseAudioFrame(badFlags).error?.reason).toBe("flags");
  });

  test("raw LE byte input is accepted with samples validation", () => {
    const samples = samplesFor(20);
    const bytes = new Uint8Array(samples * 2);
    const dv = new DataView(bytes.buffer);
    dv.setInt16(0, 1234, true);
    const wire = encodeAudioFrame(bytes, 20, 3);
    const parsed = parseAudioFrame(wire);
    expect(parsed.error).toBeUndefined();
    expect(parsed.frame?.pcm[0]).toBe(1234);
    // wrong length throws before encoding
    expect(() => encodeAudioFrame(new Uint8Array(10), 20, 0)).toThrow(/payload length/);
  });

  test("seq wraps into u32 BE; full duration family encoded", () => {
    for (const ms of [10, 20, 30, 40, 60] as const) {
      const wire = encodeAudioFrame(new Int16Array(samplesFor(ms)), ms, 5_000);
      const dv = new DataView(wire.buffer, wire.byteOffset, wire.byteLength);
      expect(dv.getUint16(8, false)).toBe(ms);
      expect(dv.getUint32(4, false)).toBe(5_000);
    }
  });
});

describe("§6 opus framing (codec 2; client framing only)", () => {
  // Committed deterministic Opus packet fixture (generated offline from a
  // deterministic 20 ms PCM frame). The same bytes live in the service's
  // `src/service/opus.rs` decode test; decode stays native (opus-rs).
  const OPUS_FIXTURE_PACKET = Uint8Array.from([
    0x48, 0x84, 0x27, 0x24, 0x8c, 0xd0, 0xdc, 0x2f, 0x41, 0xbf, 0x36, 0xdd, 0xa7, 0xbb, 0x50,
    0xb5, 0xf7, 0xae, 0x00, 0x33, 0xf7, 0x76, 0x5a, 0xa2, 0xf7, 0x90, 0xa3, 0xa0, 0x24, 0xd0,
    0xe6, 0xa1, 0x9b, 0x57, 0x6f, 0xec, 0xbb, 0x58, 0x25, 0x72, 0xf4, 0xbb, 0x2b, 0x2d, 0x86,
    0xdb, 0x07, 0x59, 0xa0, 0x16, 0x4d, 0x30, 0xa6, 0x42, 0x3d, 0x53, 0x17, 0xb0, 0x4e, 0x55,
    0x94, 0x10, 0x05, 0x03, 0x61, 0x00, 0x3b, 0x79, 0x88, 0x66, 0x41, 0x0d, 0x23, 0x73, 0xf4,
    0x04, 0x61, 0xa0, 0x9e, 0xdb, 0xfe, 0x84, 0xe3, 0x4d, 0x5d, 0x27, 0xb5, 0xaa, 0xe5, 0x0f,
    0xfd, 0xb7, 0xdd, 0xb8, 0x29, 0x53, 0x59, 0x7d, 0x1f, 0x71, 0xb6, 0xd6, 0x62, 0xf0, 0x1d,
    0x3d, 0x71, 0x74, 0x05, 0x22, 0xc5, 0x01, 0x8e, 0xde, 0x7f, 0xc1, 0x2a, 0x00, 0xc3, 0xf1,
    0xeb, 0xe3, 0x81, 0x77, 0x8d, 0xd2, 0xe4, 0x95, 0x23, 0x85, 0xe3, 0x78, 0x45, 0x88, 0xa5,
    0x85, 0x9b, 0xb4, 0x3c, 0xe2, 0x60,
  ]);

  test("fixture packet frames as a codec-2 20 ms frame", () => {
    const wire = encodeOpusFrame(OPUS_FIXTURE_PACKET, 20, 0, false);
    expect(wire.byteLength).toBe(AUDIO_HEADER_BYTES + OPUS_FIXTURE_PACKET.byteLength);
    expect(wire[0]).toBe(0x56);
    expect(wire[1]).toBe(0x58);
    expect(wire[2]).toBe(1);
    expect(wire[3]).toBe(CODEC_OPUS);
    const dv = new DataView(wire.buffer, wire.byteOffset, wire.byteLength);
    expect(dv.getUint32(4, false)).toBe(0);
    expect(dv.getUint16(8, false)).toBe(20);
    expect(dv.getUint16(10, false)).toBe(0);
    // Payload path is the packet verbatim.
    expect(
      wire.subarray(AUDIO_HEADER_BYTES).every((b, i) => b === OPUS_FIXTURE_PACKET[i]),
    ).toBe(true);
  });

  test("opus packet size is validated (RFC 6716)", () => {
    expect(
      () => encodeOpusFrame(new Uint8Array(0), 20, 0),
    ).toThrow(/opus packet length/);
    expect(() => encodeOpusFrame(new Uint8Array(OPUS_MAX_PACKET_BYTES + 1), 20, 0)).toThrow(
      /opus packet length/,
    );
    const max = new Uint8Array(OPUS_MAX_PACKET_BYTES);
    expect(encodeOpusFrame(max, 10, 9).byteLength).toBe(AUDIO_HEADER_BYTES + OPUS_MAX_PACKET_BYTES);
  });
});
