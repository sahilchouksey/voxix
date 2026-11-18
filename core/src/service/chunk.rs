//! Explicit chunking for long-audio batch transcription.
//!
//! The Cohere/transcribe.cpp batch path degrades into repetition loops on
//! inputs longer than ~40–60 s, so any audio longer than
//! [`MAX_CHUNK_SAMPLES`] is split into chunks of at most 40 s cut at
//! low-energy boundaries. Chunks are transcribed sequentially by the caller
//! and joined with a single space.
//!
//! Audio layout: 16 kHz mono f32 samples (the same produced by
//! `decode_wav_to_mono16k`).

pub const SAMPLE_RATE_HZ: usize = 16_000;
/// Hard upper bound for any emitted chunk (40 s).
pub const MAX_CHUNK_SAMPLES: usize = 40 * SAMPLE_RATE_HZ;
/// Lower bound for any emitted chunk (0.25 s); smaller tail remainders are
/// folded back into the previous cut.
pub const MIN_CHUNK_SAMPLES: usize = SAMPLE_RATE_HZ / 4;
/// First cut search starts 25 s into the chunk window.
const SEARCH_OFFSET_SAMPLES: usize = 25 * SAMPLE_RATE_HZ;
/// Energy-frame length for the search (50 ms).
const FRAME_SAMPLES: usize = SAMPLE_RATE_HZ / 20;
/// Analysis hop between frames (25 ms).
const HOP_SAMPLES: usize = SAMPLE_RATE_HZ / 40;
/// A "quiet" decision unit: 500 ms expressed in 25 ms hops.
const QUIET_WINDOW_FRAMES: usize = 20;
/// If the quietest 500 ms window's energy is within 20% of the median
/// window energy, there is effectively no pause — hard-cut at 40 s.
const NO_PAUSE_RATIO: f32 = 0.8;

/// Compute the (start, end) sample bounds of the chunks for `audio`.
///
/// Audio of at most [`MAX_CHUNK_SAMPLES`] yields exactly one chunk covering
/// the input (byte-identical behavior for short clips). Longer audio is
/// split greedily: each chunk ends at the center of the quietest 500 ms
/// window found in `[cursor + 25 s, cursor + 40 s]`, or at 40 s when no
/// pause stands out from the median energy of the window. The final chunk
/// may be shorter than 40 s. No emitted chunk is smaller than
/// [`MIN_CHUNK_SAMPLES`] or larger than [`MAX_CHUNK_SAMPLES`] (a sub-minimum
/// tail is folded into the previous chunk).
pub fn chunk_bounds(audio: &[f32]) -> Vec<(usize, usize)> {
    let len = audio.len();
    if len <= MAX_CHUNK_SAMPLES {
        return if len == 0 { Vec::new() } else { vec![(0, len)] };
    }

    let mut bounds: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0usize;
    while len - cursor > MAX_CHUNK_SAMPLES {
        let hard_end = cursor + MAX_CHUNK_SAMPLES;
        let mut cut = find_quiet_cut(audio, cursor, hard_end, len);
        if len - cut < MIN_CHUNK_SAMPLES {
            // Snap the cut back so the tail remainder stays ≥ 0.25 s.
            cut = len
                .saturating_sub(MIN_CHUNK_SAMPLES)
                .max(cursor + MIN_CHUNK_SAMPLES);
        }
        cut = cut.clamp(cursor + MIN_CHUNK_SAMPLES, hard_end);
        bounds.push((cursor, cut));
        cursor = cut;
    }
    if cursor < len {
        if len - cursor < MIN_CHUNK_SAMPLES && !bounds.is_empty() {
            // Fold a sub-minimum tail remainder into the previous chunk.
            // (Unreachable given the snap-back above; kept as a guard.)
            if let Some(last) = bounds.last_mut() {
                last.1 = len;
                return bounds;
            }
        }
        bounds.push((cursor, len));
    }
    bounds
}

/// RMS of one frame slice.
fn frame_rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let mut acc = 0.0f64;
    for &s in frame {
        acc += (s as f64) * (s as f64);
    }
    (acc / frame.len() as f64).sqrt() as f32
}

/// Pick the cut sample in `(cursor, hard_end]`: center of the quietest
/// 500 ms window in the search region, or the 40 s hard cut when the
/// quietest window is within 20% of the median window energy.
fn find_quiet_cut(audio: &[f32], cursor: usize, hard_end: usize, _len: usize) -> usize {
    let search_start = (cursor + SEARCH_OFFSET_SAMPLES).min(hard_end);
    let search_end = hard_end;

    // Per-frame energies: 50 ms frames, 25 ms hop.
    let mut frames: Vec<(usize, f32)> = Vec::new();
    let mut f = search_start;
    while f + FRAME_SAMPLES <= search_end {
        frames.push((f, frame_rms(&audio[f..f + FRAME_SAMPLES])));
        f += HOP_SAMPLES;
    }
    if frames.len() < QUIET_WINDOW_FRAMES {
        // Not enough room for even one 500 ms decision window — hard cut.
        return hard_end;
    }

    // Aggregate per-frame energies into sliding 500 ms windows.
    let n_windows = frames.len() - QUIET_WINDOW_FRAMES + 1;
    let mut prefix = vec![0.0f64; frames.len() + 1];
    for (i, &(_, e)) in frames.iter().enumerate() {
        prefix[i + 1] = prefix[i] + e as f64;
    }
    let mut window_energy = Vec::with_capacity(n_windows);
    for w in 0..n_windows {
        let mean = (prefix[w + QUIET_WINDOW_FRAMES] - prefix[w]) / QUIET_WINDOW_FRAMES as f64;
        window_energy.push(mean as f32);
    }

    let mut sorted = window_energy.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    let (min_idx, &min_e) = window_energy
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .expect("non-empty window_energy");

    if min_e > median * NO_PAUSE_RATIO {
        // No pause stands out — hard cut at 40 s.
        return hard_end;
    }

    // Cut at the center of the quietest 500 ms window.
    let first = frames[min_idx].0;
    let last_frame_start = frames[min_idx + QUIET_WINDOW_FRAMES - 1].0;
    let window_end = last_frame_start + FRAME_SAMPLES;
    (first + window_end) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silence(n: usize) -> Vec<f32> {
        vec![0.0; n]
    }

    #[test]
    fn short_audio_is_single_chunk() {
        let audio = silence(SAMPLE_RATE_HZ * 15);
        assert_eq!(chunk_bounds(&audio), vec![(0, audio.len())]);
    }

    #[test]
    fn exactly_max_is_single_chunk() {
        let audio = silence(MAX_CHUNK_SAMPLES);
        assert_eq!(chunk_bounds(&audio), vec![(0, MAX_CHUNK_SAMPLES)]);
    }

    #[test]
    fn long_constant_energy_hard_cuts_at_40s() {
        // Steady non-zero energy: no pause exists, so cuts land at 40 s.
        let audio = vec![0.5f32; SAMPLE_RATE_HZ * 100];
        let bounds = chunk_bounds(&audio);
        assert_eq!(bounds[0], (0, MAX_CHUNK_SAMPLES));
        assert_eq!(bounds[1], (MAX_CHUNK_SAMPLES, 2 * MAX_CHUNK_SAMPLES));
        assert_eq!(bounds[2], (2 * MAX_CHUNK_SAMPLES, audio.len()));
        for &(s, e) in &bounds {
            assert!(e - s <= MAX_CHUNK_SAMPLES);
            assert!(e - s >= MIN_CHUNK_SAMPLES);
        }
    }

    #[test]
    fn quiet_boundary_is_preferred() {
        // Loud audio with a silent gap at 33 s: the cut should land inside it.
        let mut audio = vec![0.5f32; SAMPLE_RATE_HZ * 100];
        for s in audio
            .iter_mut()
            .take(SAMPLE_RATE_HZ * 34)
            .skip(SAMPLE_RATE_HZ * 33)
        {
            *s = 0.0;
        }
        let bounds = chunk_bounds(&audio);
        let cut = bounds[0].1;
        assert!(
            cut >= SAMPLE_RATE_HZ * 33 && cut <= SAMPLE_RATE_HZ * 34,
            "cut {cut} not inside the silent gap"
        );
        assert!(bounds[0].1 - bounds[0].0 <= MAX_CHUNK_SAMPLES);
        assert_eq!(bounds.last().unwrap().1, audio.len());
    }

    #[test]
    fn no_chunk_exceeds_limits() {
        let audio = vec![0.25f32; SAMPLE_RATE_HZ * 207];
        let bounds = chunk_bounds(&audio);
        let mut covered = 0usize;
        for &(s, e) in &bounds {
            assert_eq!(s, covered);
            assert!(e - s <= MAX_CHUNK_SAMPLES);
            assert!(e - s >= MIN_CHUNK_SAMPLES);
            covered = e;
        }
        assert_eq!(covered, audio.len());
    }
}
