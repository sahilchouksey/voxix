/** PCM16 conversion helpers shared by the web capture path (session pushes
 * decode as i16/32768.0, §6). */

/** Clamp [-1..1] + scale to i16. */
export function packPcm16(samples: number[]): Int16Array {
  const out = new Int16Array(samples.length);
  for (let i = 0; i < samples.length; i++) {
    const v = Math.round(samples[i] * 32768);
    out[i] = v < -32768 ? -32768 : v > 32767 ? 32767 : v;
  }
  return out;
}

/** Clamp [-1..1] + scale to i16 from a Float32Array. */
export function packPcm16FromFloat(samples: Float32Array): Int16Array {
  const out = new Int16Array(samples.length);
  for (let i = 0; i < samples.length; i++) {
    const v = Math.round(samples[i] * 32768);
    out[i] = v < -32768 ? -32768 : v > 32767 ? 32767 : v;
  }
  return out;
}

/** Merge interleaved multi-channel input into a single mono f32 frame. */
export function downmixToMono(
  interleaved: Float32Array,
  channels: number,
): Float32Array {
  const frames = interleaved.length / channels;
  const mono = new Float32Array(frames);
  for (let i = 0; i < frames; i++) {
    let sum = 0;
    for (let c = 0; c < channels; c++) sum += interleaved[i * channels + c];
    mono[i] = sum / channels;
  }
  return mono;
}

/** 32768 divisor matches the service's i16→f32 decode exactly. */
export const PCM_SCALE = 32768;
