//! Optional Opus codec for the v1 audio plane (SDK plan §6; default off).
//!
//! Gated behind the `service-opus` Cargo feature: while the feature is off the
//! service negotiates and validates only the mandatory `pcm_s16le` baseline, and
//! `PATCH /v1/config` answers 422 when a client asks to enable the codec. The
//! feature-on path accepts the negotiated `opus` codec end to end — frame
//! validation plus decode to the 16 kHz mono PCM16 the engine consumes.

use crate::service::audio::{
    frame_payload_samples, AudioFrame, FrameError, AUDIO_HEADER_BYTES, FLAG_DISCONTINUITY,
    LEGAL_FRAME_DURATIONS,
};

/// The negotiated codec name for the optional Opus codec.
pub const OPUS_CODEC_NAME: &str = "opus";
/// RFC 6716 §3.1: a single Opus packet carries at most 1276 bytes of data.
pub const OPUS_MAX_PACKET_BYTES: usize = 1276;
/// RFC 6716: max frames encoded into one Opus packet (48 for 2.5 ms codes).
pub const OPUS_MAX_PACKET_FRAMES: usize = 48;
/// Opus operates at 16 kHz mono for the v1 audio profile (SDK plan §5/§6).
pub const OPUS_SAMPLE_RATE_HZ: i32 = 16_000;
pub const OPUS_CHANNELS: usize = 1;

/// Stateful Opus decoder (one per session audio plane; the reader task owns it).
pub struct OpusDecoder {
    inner: opus_rs::OpusDecoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, &'static str> {
        Ok(Self {
            inner: opus_rs::OpusDecoder::new(OPUS_SAMPLE_RATE_HZ, OPUS_CHANNELS)?,
        })
    }

    /// Decode one Opus packet into interleaved f32 PCM at the API rate; the
    /// caller sized the output for the frame duration.
    pub fn decode(
        &mut self,
        packet: &[u8],
        frame_size: usize,
        out: &mut [f32],
    ) -> Result<usize, &'static str> {
        self.inner.decode(packet, frame_size, out)
    }
}

/// Opus encoder (one per process audio plane; used for offline/deterministic
/// fixtures and server-side packet production while the feature is on).
pub struct OpusEncoder {
    inner: opus_rs::OpusEncoder,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, &'static str> {
        Ok(Self {
            inner: opus_rs::OpusEncoder::new(
                OPUS_SAMPLE_RATE_HZ,
                OPUS_CHANNELS,
                opus_rs::Application::Voip,
            )?,
        })
    }

    /// Encode one PCM16 (`opus_int16` convention) frame into a single Opus
    /// packet. Returns the encoded packet length in bytes.
    pub fn encode_i16_packet(
        &mut self,
        pcm: &[i16],
        frame_size: usize,
        output: &mut [u8],
    ) -> Result<usize, &'static str> {
        self.inner.encode_i16(pcm, frame_size, output)
    }
}

/// Validate one negotiated-opus binary frame and decode it through the
/// session's decoder into the PCM16 samples the engine consumes. Frame errors
/// map onto the shared `FrameError` vocabulary (→ `invalid_request` / 1008).
pub fn decode_opus_frame(
    decoder: &mut OpusDecoder,
    bytes: &[u8],
) -> Result<AudioFrame, FrameError> {
    if bytes.len() < AUDIO_HEADER_BYTES {
        return Err(FrameError::Short);
    }
    if bytes[0] != b'V' || bytes[1] != b'X' {
        return Err(FrameError::Magic);
    }
    if bytes[2] != 1 {
        return Err(FrameError::Version);
    }
    if bytes[3] != 2 {
        // The negotiated codec is opus; a pcm_s16le header is a mismatch.
        return Err(FrameError::Codec);
    }
    let seq = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as u64;
    let duration_ms = u16::from_be_bytes([bytes[8], bytes[9]]);
    let flags = u16::from_be_bytes([bytes[10], bytes[11]]);
    if !LEGAL_FRAME_DURATIONS.contains(&duration_ms) {
        return Err(FrameError::Duration);
    }
    if flags & !FLAG_DISCONTINUITY != 0 {
        return Err(FrameError::Flags);
    }
    let packet = &bytes[AUDIO_HEADER_BYTES..];
    if packet.is_empty() || packet.len() > OPUS_MAX_PACKET_BYTES {
        return Err(FrameError::Opus(format!(
            "opus frame payload must be 1..={} bytes, got {}",
            OPUS_MAX_PACKET_BYTES,
            packet.len()
        )));
    }
    let frame_size = frame_payload_samples(duration_ms);
    let mut out = vec![0.0f32; frame_size * OPUS_CHANNELS];
    let decoded = decoder
        .decode(packet, frame_size, &mut out)
        .map_err(|e| FrameError::Opus(e.to_string()))?;
    let samples = decoded.min(frame_size * OPUS_CHANNELS);
    let mut pcm = Vec::with_capacity(samples);
    for value in out[..samples].iter() {
        let quantized = (*value * 32768.0).round() as i16;
        pcm.push(quantized.clamp(-32768, 32767));
    }
    Ok(AudioFrame {
        seq,
        duration_ms,
        discontinuity: flags & FLAG_DISCONTINUITY != 0,
        pcm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Committed deterministic Opus packet fixture (generated offline from a
    /// deterministic 20 ms PCM frame; RFC 6716 packet, ≤ 1276 bytes). The bytes
    /// are a stable round-trip vector while the `service-opus` feature is on.
    const OPUS_FIXTURE_PACKET: [u8; 141] = [
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
    ];

    #[test]
    fn opus_committed_fixture_decodes_to_frame_pcm() {
        let mut decoder = OpusDecoder::new().expect("opus decoder init");
        let frame_size = (OPUS_SAMPLE_RATE_HZ as usize * 20 / 1000) as usize;
        let mut out = vec![0.0f32; frame_size];
        let decoded = decoder
            .decode(&OPUS_FIXTURE_PACKET, frame_size, &mut out)
            .expect("opus decode of the committed fixture");
        assert_eq!(decoded, frame_size);
        assert!(
            out.iter().any(|v| (*v).abs() > 0.01),
            "fixture frame carries audio"
        );
    }
}
