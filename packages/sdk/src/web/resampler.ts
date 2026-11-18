/**
 * Stateful streaming resampler: arbitrary `sourceRateHz` → 16 kHz mono,
 * platform-neutral (no DOM). Incremental linear interpolation between the
 * two most recent input samples; output lands exactly chunked for the
 * service profile (20 ms ⇒ 320 samples).
 */
import { SAMPLE_RATE_HZ } from "../protocol/audio.js";
import { packPcm16 } from "./pcm.js";

export interface ResamplerOptions {
  sourceRateHz: number;
  targetRateHz?: number;
  chunkMs?: 10 | 20 | 30 | 40 | 60;
}

export class RtResampler {
  readonly sourceRateHz: number;
  readonly targetRateHz: number;
  readonly chunkMs: 10 | 20 | 30 | 40 | 60;
  /** Output samples per emitted frame (fixed-size Int16Arrays). */
  readonly targetChunkSamples: number;
  /** Source samples consumed per source sample (sourceRate/targetRate). */
  readonly ratio: number;
  /** Fractional position (in input-sample units) of the next output point. */
  private phase = 0;
  private prev = 0;
  private pending: number[] = [];

  constructor(options: ResamplerOptions) {
    this.sourceRateHz = options.sourceRateHz;
    this.targetRateHz = options.targetRateHz ?? 16_000;
    this.chunkMs = options.chunkMs ?? 20;
    if (!(this.sourceRateHz > 0) || this.sourceRateHz >= 384_000) {
      throw new Error(`unsupported source rate ${this.sourceRateHz}`);
    }
    if (!(this.targetRateHz > 0))
      throw new Error(`unsupported target rate ${this.targetRateHz}`);
    this.ratio = this.sourceRateHz / this.targetRateHz;
    this.targetChunkSamples = Math.round(
      (SAMPLE_RATE_HZ * this.chunkMs) / 1000,
    );
  }

  /**
   * Consume a block of mono f32 [-1..1] (already downmixed by the caller)
   * and drain every complete target-rate frame.
   */
  push(chunk: Float32Array): Int16Array[] {
    const frames: Int16Array[] = [];
    for (let i = 0; i < chunk.length; i++) {
      const current = chunk[i];
      // Emit every output point strictly inside [prev, current).
      while (this.phase < 1) {
        this.pending.push(this.prev + (current - this.prev) * this.phase);
        if (this.pending.length === this.targetChunkSamples) {
          frames.push(packPcm16(this.pending));
          this.pending = [];
        }
        this.phase += this.ratio;
      }
      this.phase -= 1;
      this.prev = current;
    }
    return frames;
  }

  /** Zero-order hold across the trailing fractional region; optional. */
  flush(): Int16Array | undefined {
    while (this.phase < 1) {
      this.pending.push(this.prev);
      if (this.pending.length === this.targetChunkSamples) {
        this.phase = 1; // abandon any further region
        break;
      }
      this.phase += this.ratio;
    }
    if (this.pending.length === 0) return undefined;
    while (this.pending.length < this.targetChunkSamples) this.pending.push(0);
    const frame = packPcm16(this.pending);
    this.pending = [];
    return frame;
  }
}

/** One-shot whole-buffer resample into fixed-size Int16Array frames. */
export function downmixAndResample(
  mono: Float32Array,
  options: ResamplerOptions,
): Int16Array[] {
  const resampler = new RtResampler(options);
  const frames = resampler.push(mono);
  const tail = resampler.flush();
  if (tail) frames.push(tail);
  return frames;
}

/** Target-rate sample count for a chunk duration (16 kHz mono). */
export function targetSamplesFor(durationMs: 10 | 20 | 30 | 40 | 60): number {
  return Math.round((SAMPLE_RATE_HZ * durationMs) / 1000);
}
