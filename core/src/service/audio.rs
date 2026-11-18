//! Per-session audio pipeline (SDK plan §6/§3.4/§11).
//!
//! - The §6 binary frame header imported exactly as numbered there: `VX`
//!   magic, version 1, codec `1=pcm_s16le`, big-endian u32 audio sequence
//!   (starts 0), big-endian u16 frame duration (10/20/30/40/60), big-endian
//!   u16 flags (bit 0 = discontinuity, others zero), then little-endian
//!   signed 16-bit mono samples at 16 kHz.
//! - PCM `i16/32768.0 → f32` conversion, duplicate-sequence acceptance,
//!   bounded silence synthesis on flagged discontinuity, `audio_gap`
//!   otherwise.
//! - Server-canonical VAD reuses the existing `audio_toolkit/vad` wiring
//!   (Silero inside the streaming-hangover `SmoothedVad`, 30 ms frames; the
//!   20 ms network frames are re-batched, never re-implemented), gates the
//!   audio the decoder receives (speech-only, as the desktop recorder does),
//!   and reports speech boundaries for `speech_start`/`speech_end` plus the
//!   `auto_finalize_ms` countdown (§3.4/§15 G7).
//! - The bounded normalized-audio accumulator is the WAV-accumulator role
//!   from §12 M2: the final-only fallback decodes exactly what was received,
//!   capped by the session duration bound.

use crate::audio_toolkit::vad::{
    SileroVad, SmoothedVad, VoiceActivityDetector, VAD_ONSET_FRAMES, VAD_PREFILL_FRAMES,
    VAD_STREAMING_HANGOVER_FRAMES,
};
use anyhow::Result;

pub const SAMPLE_RATE_HZ: u32 = 16_000;
pub const VAD_FRAME_MS: u32 = 30;
pub const VAD_FRAME_SAMPLES: usize = (SAMPLE_RATE_HZ as usize) * VAD_FRAME_MS as usize / 1000;
/// Audio frame codec identifiers (§6 binary header): `1 = pcm_s16le` (mandatory
/// baseline), `2 = opus` (negotiated when the `service-opus` feature is on).
pub const CODEC_PCM_S16LE: u8 = 1;
pub const CODEC_OPUS: u8 = 2;
pub const AUDIO_HEADER_BYTES: usize = 12;
pub const FLAG_DISCONTINUITY: u16 = 1;
/// Legal frame durations per §6 binary header table.
pub const LEGAL_FRAME_DURATIONS: [u16; 5] = [10, 20, 30, 40, 60];

/// A parsed client audio frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub seq: u64,
    pub duration_ms: u16,
    pub discontinuity: bool,
    pub pcm: Vec<i16>,
}

/// Frame-level rejection reasons (map to `invalid_request` envelopes + 1008).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Short,
    Magic,
    Version,
    Codec,
    Duration,
    Flags,
    PayloadLength {
        expected_samples: usize,
        got: usize,
    },
    /// Opus packet framing/decode failures (feature-gated codec path).
    Opus(String),
}

impl FrameError {
    pub fn message(&self) -> String {
        match self {
            FrameError::Short => "audio frame shorter than the 12-byte header".to_string(),
            FrameError::Magic => "audio frame missing the 'VX' magic".to_string(),
            FrameError::Version => "unsupported audio frame version (only 1)".to_string(),
            FrameError::Codec => "unsupported audio codec".to_string(),
            FrameError::Duration => "unsupported frame duration (10/20/30/40/60 ms)".to_string(),
            FrameError::Flags => "reserved flag bits set".to_string(),
            FrameError::PayloadLength {
                expected_samples,
                got,
            } => format!(
                "payload length does not match duration: expected {expected_samples} samples, got {got}"
            ),
            FrameError::Opus(message) => format!("opus audio frame rejected: {message}"),
        }
    }
}

pub fn frame_payload_samples(duration_ms: u16) -> usize {
    (SAMPLE_RATE_HZ as usize) * duration_ms as usize / 1000
}

/// Parse the fixed §6 binary frame.
pub fn parse_audio_frame(bytes: &[u8]) -> Result<AudioFrame, FrameError> {
    if bytes.len() < AUDIO_HEADER_BYTES {
        return Err(FrameError::Short);
    }
    if bytes[0] != b'V' || bytes[1] != b'X' {
        return Err(FrameError::Magic);
    }
    if bytes[2] != 1 {
        return Err(FrameError::Version);
    }
    if bytes[3] != CODEC_PCM_S16LE {
        return Err(FrameError::Codec);
    }
    let seq = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let duration_ms = u16::from_be_bytes([bytes[8], bytes[9]]);
    let flags = u16::from_be_bytes([bytes[10], bytes[11]]);
    if !LEGAL_FRAME_DURATIONS.contains(&duration_ms) {
        return Err(FrameError::Duration);
    }
    if flags & !FLAG_DISCONTINUITY != 0 {
        return Err(FrameError::Flags);
    }
    let payload = &bytes[AUDIO_HEADER_BYTES..];
    let expected = frame_payload_samples(duration_ms);
    if payload.len() % 2 != 0 || payload.len() / 2 != expected {
        return Err(FrameError::PayloadLength {
            expected_samples: expected,
            got: payload.len() / 2,
        });
    }
    let pcm = payload
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(AudioFrame {
        seq: seq as u64,
        duration_ms,
        discontinuity: flags & FLAG_DISCONTINUITY != 0,
        pcm,
    })
}

/// Serialize a frame back to its §6 wire shape (tests + golden vectors).
pub fn encode_audio_frame(frame: &AudioFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(AUDIO_HEADER_BYTES + frame.pcm.len() * 2);
    out.push(b'V');
    out.push(b'X');
    out.push(1);
    out.push(CODEC_PCM_S16LE);
    out.extend_from_slice(&(frame.seq as u32).to_be_bytes());
    out.extend_from_slice(&frame.duration_ms.to_be_bytes());
    let flags = if frame.discontinuity {
        FLAG_DISCONTINUITY
    } else {
        0
    };
    out.extend_from_slice(&flags.to_be_bytes());
    for sample in &frame.pcm {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// `i16 / 32768.0 → f32` per §6.
pub fn pcm_to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|s| *s as f32 / 32768.0).collect()
}

/// Zero-length silence of `duration_ms` at 16 kHz mono f32.
pub fn silence_f32(duration_ms: u64) -> Vec<f32> {
    vec![0.0; (SAMPLE_RATE_HZ as usize) * (duration_ms as usize) / 1000]
}

/// Merge arbitrary input chunk durations into the exact 30 ms frames the
/// canonical VAD consumes (§6: "server re-batches to 30 ms for existing
/// VAD"). The carry never exceeds one frame.
pub struct Rebatcher {
    carry: Vec<f32>,
}

impl Rebatcher {
    pub fn new() -> Self {
        Rebatcher { carry: Vec::new() }
    }

    /// Push converted samples; returns complete 30 ms frames in order.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        self.carry.extend_from_slice(samples);
        while self.carry.len() >= VAD_FRAME_SAMPLES {
            let frame: Vec<f32> = self.carry.drain(..VAD_FRAME_SAMPLES).collect();
            out.push(frame);
        }
        out
    }

    /// Samples waiting to complete a 30 ms frame.
    pub fn carry_len(&self) -> usize {
        self.carry.len()
    }
}

/// Boundary events surfaced for the WS layer (§6 `speech_start`/`speech_end`,
/// §15 G7 `finalizing {source:"vad"}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    SpeechStarted { audio_ms: u64 },
    SpeechEnded { audio_ms: u64 },
}

/// Effects of feeding one frame through the pipeline.
#[derive(Debug, Default)]
pub struct VadEffects {
    /// Gated audio the decoder must receive, in order (speech-only in server
    /// mode; the entire frame otherwise).
    pub engine_chunks: Vec<Vec<f32>>,
    pub events: Vec<VadEvent>,
    /// Set when the silence counter passed `auto_finalize_ms` this frame.
    pub auto_finalize: bool,
}

/// The session's server-VAD wrapper. `client`/`disabled` modes bypass the
/// detector entirely (speech-only input / all-input respectively) and never
/// emit boundaries (§3.4).
pub struct SessionVad {
    mode: crate::service::api::VadMode,
    detector: Option<Box<dyn VoiceActivityDetector>>,
    in_speech: bool,
    /// Whether speech was ever seen (§3.4: `auto_finalize_ms` is silence
    /// AFTER speech; pre-speech silence never consumes the budget).
    saw_speech: bool,
    silence_ms: u64,
    audio_ms: u64,
    rebatch: Rebatcher,
    auto_finalize_ms: Option<u64>,
    auto_fired: bool,
}

impl SessionVad {
    /// Reuse — do not fork — the existing detector stack: one Silero engine
    /// under the recorder's smoothing wrapper with the streaming hangover.
    pub fn server(
        vad_model_path: &std::path::Path,
        threshold: f32,
        auto_finalize_ms: Option<u64>,
    ) -> Result<Self> {
        let silero = SileroVad::new(vad_model_path, threshold)?;
        let smoothed = SmoothedVad::new(
            Box::new(silero),
            VAD_PREFILL_FRAMES,
            VAD_STREAMING_HANGOVER_FRAMES,
            VAD_ONSET_FRAMES,
        );
        Ok(SessionVad::with_detector(
            crate::service::api::VadMode::Server,
            Some(Box::new(smoothed)),
            auto_finalize_ms,
        ))
    }

    pub fn bypass(mode: crate::service::api::VadMode) -> Self {
        SessionVad::with_detector(mode, None, None)
    }

    pub fn with_detector(
        mode: crate::service::api::VadMode,
        detector: Option<Box<dyn VoiceActivityDetector>>,
        auto_finalize_ms: Option<u64>,
    ) -> Self {
        SessionVad {
            mode,
            detector,
            in_speech: false,
            saw_speech: false,
            silence_ms: 0,
            audio_ms: 0,
            rebatch: Rebatcher::new(),
            auto_finalize_ms,
            auto_fired: false,
        }
    }

    pub fn mode(&self) -> crate::service::api::VadMode {
        self.mode
    }

    pub fn audio_ms(&self) -> u64 {
        self.audio_ms
    }

    /// Reset the detector on flagged discontinuity (new acoustic context).
    pub fn reset(&mut self) {
        if let Some(detector) = self.detector.as_mut() {
            detector.reset();
        }
        self.in_speech = false;
        self.silence_ms = 0;
        self.saw_speech = false;
        self.rebatch = Rebatcher::new();
    }

    /// Feed one frame's samples; returns the effects for this frame.
    pub fn push(&mut self, samples: &[f32]) -> VadEffects {
        let duration_ms = (samples.len() as u64 * 1000) / SAMPLE_RATE_HZ as u64;
        self.audio_ms += duration_ms;
        let mut effects = VadEffects::default();

        if self.mode != crate::service::api::VadMode::Server {
            effects.engine_chunks.push(samples.to_vec());
            return effects;
        }

        if self.detector.is_none() {
            effects.engine_chunks.push(samples.to_vec());
            return effects;
        }

        for chunk in self.rebatch.push(samples) {
            let Some(chunk_speech) = (|| {
                let detector = self.detector.as_mut()?;
                let voiced = detector
                    .push_frame(&chunk)
                    .map(|frame| frame.is_speech())
                    .unwrap_or(true);
                Some(voiced)
            })() else {
                break;
            };
            if chunk_speech {
                if !self.in_speech {
                    self.in_speech = true;
                    self.saw_speech = true;
                    effects.events.push(VadEvent::SpeechStarted {
                        audio_ms: self.audio_ms,
                    });
                }
                self.silence_ms = 0;
                effects.engine_chunks.push(chunk);
            } else {
                // Noise 30 ms frames are dropped; the trailing silence budget
                // (§3.4) counts only after speech exists.
                if self.saw_speech {
                    self.silence_ms += VAD_FRAME_MS as u64;
                }
                if self.in_speech {
                    // Leaving speech now is the real boundary; the trailing
                    // auto-finalize budget starts fresh at it.
                    self.in_speech = false;
                    self.silence_ms = 0;
                    effects.events.push(VadEvent::SpeechEnded {
                        audio_ms: self.audio_ms,
                    });
                }
            }
        }

        if !self.auto_fired {
            if let Some(limit) = self.auto_finalize_ms {
                if self.silence_ms >= limit {
                    self.auto_fired = true;
                    effects.auto_finalize = true;
                }
            }
        }
        effects
    }
}

/// Bounded normalized-audio accumulator (utterance recording for the
/// final-only authoritative decode and, later, history writes).
pub struct AudioAccumulator {
    samples: Vec<f32>,
    cap_samples: usize,
}

impl AudioAccumulator {
    pub fn with_cap_seconds(seconds: u64) -> Self {
        AudioAccumulator {
            samples: Vec::new(),
            cap_samples: (seconds as usize) * SAMPLE_RATE_HZ as usize,
        }
    }

    /// Push samples; `false` when the accumulator is full and the input was
    /// refused (callers surface the duration-limit policy error, §4.5).
    pub fn push(&mut self, samples: &[f32]) -> bool {
        if self.samples.len() + samples.len() > self.cap_samples {
            return false;
        }
        self.samples.extend_from_slice(samples);
        true
    }

    pub fn is_full(&self) -> bool {
        self.samples.len() >= self.cap_samples
    }

    pub fn len_samples(&self) -> usize {
        self.samples.len()
    }

    pub fn len_ms(&self) -> u64 {
        (self.samples.len() as u64 * 1000) / SAMPLE_RATE_HZ as u64
    }

    pub fn into_inner(self) -> Vec<f32> {
        self.samples
    }

    pub fn snapshot(&self) -> &[f32] {
        &self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_toolkit::vad::VadFrame;

    const TEST_VAD_MODEL: &str = "crates/vad-rs/tests/fixtures/silero_vad_v4.onnx";

    fn frame(seq: u64, duration_ms: u16, pcm: Vec<i16>) -> Vec<u8> {
        encode_audio_frame(&AudioFrame {
            seq,
            duration_ms,
            discontinuity: false,
            pcm,
        })
    }

    #[test]
    fn header_golden_vector_roundtrip() {
        // 20 ms = 320 samples / 640 bytes per §6; header is 12 bytes.
        let pcm = vec![0x0100i16; 320];
        let bytes = frame(7, 20, pcm.clone());
        assert_eq!(bytes.len(), 12 + 640);
        assert_eq!(&bytes[0..2], b"VX");
        assert_eq!(bytes[2], 1);
        assert_eq!(bytes[3], 1);
        assert_eq!(&bytes[2..8], &[1u8, 1, 0, 0, 0, 7]);
        assert_eq!(&bytes[8..10], &[0u8, 20]);
        assert_eq!(&bytes[10..12], &[0u8, 0]);
        assert_eq!(bytes[12], 0x00, "payload is little-endian i16");
        assert_eq!(bytes[13], 0x01);

        let parsed = parse_audio_frame(&bytes).expect("parse");
        assert_eq!(parsed.seq, 7);
        assert_eq!(parsed.duration_ms, 20);
        assert!(!parsed.discontinuity);
        assert_eq!(parsed.pcm, pcm);

        // Endianness: a high sequence number renders big-endian.
        let big = frame(0x0102_0304, 20, pcm.clone());
        assert_eq!(&big[4..8], &[0x01, 0x02, 0x03, 0x04]);
        let parsed = parse_audio_frame(&big).expect("parse");
        assert_eq!(parsed.seq, 0x0102_0304);
    }

    #[test]
    fn header_rejects_malformed_shapes() {
        assert!(matches!(
            parse_audio_frame(&[0u8; 8]),
            Err(FrameError::Short)
        ));
        assert!(matches!(
            parse_audio_frame(&[b'X', b'V', 1, 1, 0, 0, 0, 0, 0, 20, 0, 0]),
            Err(FrameError::Magic)
        ));
        assert!(matches!(
            parse_audio_frame(&[b'V', b'X', 2, 1, 0, 0, 0, 0, 0, 20, 0, 0]),
            Err(FrameError::Version)
        ));
        assert!(matches!(
            parse_audio_frame(&[b'V', b'X', 1, 2, 0, 0, 0, 0, 0, 20, 0, 0]),
            Err(FrameError::Codec)
        ));
        // Unsupported duration (25 ms not legal).
        let mut bad = frame(0, 20, vec![0; 320]);
        bad[8] = 0;
        bad[9] = 25;
        assert!(matches!(parse_audio_frame(&bad), Err(FrameError::Duration)));
        // Reserved flag bit set.
        let mut bad = frame(0, 20, vec![0; 320]);
        bad[11] = 0b10;
        assert!(matches!(parse_audio_frame(&bad), Err(FrameError::Flags)));
        // Payload shorter than the declared duration.
        assert!(matches!(
            parse_audio_frame(&frame(0, 20, vec![0; 100])),
            Err(FrameError::PayloadLength { .. })
        ));
    }

    #[test]
    fn pcm_conversion_uses_i16_over_32768() {
        // §6: i16 / 32768.0. i16::MIN maps to exactly -1.0; i16::MAX maps to
        // 0.99997 (32767/32768) — one LSB below full scale by design.
        let out = pcm_to_f32(&[i16::MIN, -16384, 0, 16384, i16::MAX]);
        assert_eq!(out[0], -1.0);
        assert!((out[1] - (-0.5)).abs() < 1e-4);
        assert_eq!(out[2], 0.0);
        assert!((out[3] - 0.5).abs() < 1e-4);
        assert!((out[4] - (32767.0 / 32768.0)).abs() < 1e-6);
    }

    #[test]
    fn rebatcher_merges_chunks_into_thirty_ms_frames() {
        let mut rebatch = Rebatcher::new();
        // 20 ms chunks re-batch to 30 ms (480-sample) VAD frames: 3 × 320 = 960
        // samples = 2 × 480 exactly.
        let first = rebatch.push(&vec![0.0; 320]);
        assert!(first.is_empty());
        assert_eq!(rebatch.carry_len(), 320);
        let second = rebatch.push(&vec![1.0; 320]);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0][0], 0.0);
        assert_eq!(rebatch.carry_len(), 160);
        let third = rebatch.push(&vec![2.0; 320]);
        assert_eq!(third.len(), 1);
        assert_eq!(third[0][0], 1.0);
        assert_eq!(rebatch.carry_len(), 0, "960 samples = 2 × 480 exactly");
        // A 10 ms leftover stays in the carry.
        let leftovers = rebatch.push(&vec![3.0; 160]);
        assert!(leftovers.is_empty());
        assert_eq!(rebatch.carry_len(), 160, "the partial frame waits");
        // Empty pushes never fabricate frames or mutate the carry.
        let none = rebatch.push(&[]);
        assert!(none.is_empty());
        assert_eq!(rebatch.carry_len(), 160);
        // The carry combines with the next chunk into the next 30 ms frame.
        let next = rebatch.push(&vec![4.0; 320]);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0][0], 3.0);
        assert_eq!(rebatch.carry_len(), 0);
    }

    #[test]
    fn rebatcher_carries_partial_frames_across() {
        let mut rebatch = Rebatcher::new();
        // 60 ms frame splits exactly into two 30 ms VAD frames.
        let out = rebatch.push(&vec![0.5; 960]);
        assert_eq!(out.len(), 2);
        assert_eq!(rebatch.carry_len(), 0);
    }

    /// Scripted detector: consults a shared atomic so the test toggles the
    /// voice state between frames.
    struct ScriptedDetector {
        speaking: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl VoiceActivityDetector for ScriptedDetector {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            Ok(
                if self.speaking.load(std::sync::atomic::Ordering::Relaxed) {
                    VadFrame::Speech(frame)
                } else {
                    VadFrame::Noise
                },
            )
        }
    }

    fn scripted_vad(
        auto_finalize_ms: Option<u64>,
    ) -> (SessionVad, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let speaking = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let detector = ScriptedDetector {
            speaking: speaking.clone(),
        };
        (
            SessionVad::with_detector(
                crate::service::api::VadMode::Server,
                Some(Box::new(detector)),
                auto_finalize_ms,
            ),
            speaking,
        )
    }

    #[test]
    fn vad_reports_boundaries_and_gates_audio() {
        let (mut session, speaking) = scripted_vad(Some(60));
        session.push(&vec![0.0; 480]); // silence 0-30 ms
        session.push(&vec![0.0; 480]); // silence 30-60 ms
        speaking.store(true, std::sync::atomic::Ordering::Relaxed);
        let start = session.push(&vec![0.5; 480]); // speech begins (~90 ms)
        assert!(matches!(
            start.events[..],
            [VadEvent::SpeechStarted { audio_ms }] if audio_ms == 90
        ));
        assert_eq!(start.engine_chunks.len(), 1);
        assert!(!start.auto_finalize);
        let held = session.push(&vec![0.5; 480]); // speech held
        assert!(held.events.is_empty());
        assert_eq!(held.engine_chunks.len(), 1);
        speaking.store(false, std::sync::atomic::Ordering::Relaxed);
        let stop = session.push(&vec![0.0; 480]); // speech ends (~150 ms)
        assert!(matches!(
            stop.events[..],
            [VadEvent::SpeechEnded { audio_ms }] if audio_ms == 150
        ));
        assert!(stop.engine_chunks.is_empty(), "noise is not fed on");
        // One further silent 30 ms frame arms the 60 ms auto-finalize.
        let silent = session.push(&vec![0.0; 480]);
        assert!(!silent.auto_finalize);
        let armed = session.push(&vec![0.0; 480]);
        assert!(armed.auto_finalize);
        // It fires once per session.
        let never = session.push(&vec![0.0; 480]);
        assert!(!never.auto_finalize);
    }

    #[test]
    fn vad_bypass_modes_feed_everything_without_events() {
        for mode in [
            crate::service::api::VadMode::Client,
            crate::service::api::VadMode::Disabled,
        ] {
            let mut session = SessionVad::bypass(mode);
            let out = session.push(&vec![0.25; 480]);
            assert_eq!(out.events.len(), 0);
            assert_eq!(out.engine_chunks.len(), 1);
            assert_eq!(out.engine_chunks[0].len(), 480);
        }
    }

    #[test]
    fn accumulator_is_bounded_by_the_session_cap() {
        let mut accum = AudioAccumulator::with_cap_seconds(1);
        assert!(accum.push(&vec![0.5; 8000]), "0.5 s fits");
        assert_eq!(accum.len_ms(), 500);
        let over = accum.push(&vec![0.5; 8000]); // the second half fits exactly to the cap
        assert!(over, "exactly at the cap still accepts");
        assert_eq!(accum.len_ms(), 1000);
        let refused = accum.push(&vec![0.5; 160]);
        assert!(
            !refused,
            "beyond the cap the input is refused, nothing stored"
        );
        assert_eq!(accum.len_ms(), 1000);
        assert!(accum.is_full());
    }

    #[test]
    fn real_silero_vad_pipeline_smoke() {
        // Reuse — do not fork — the existing Silero wiring against the in-repo
        // fixture: silence stays noise, and a reset clears the smoothing tail.
        let path = std::path::Path::new(TEST_VAD_MODEL);
        let Ok(mut session) = SessionVad::server(path, 0.3, None) else {
            eprintln!("silero fixture unavailable in this environment; skipping");
            return;
        };
        for _ in 0..4 {
            let out = session.push(&silence_f32(30));
            assert!(out.engine_chunks.is_empty(), "silence must not feed");
            assert!(out.events.is_empty());
        }
        session.reset();
        assert_eq!(session.audio_ms(), 120);
    }
}
