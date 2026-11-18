/** Web subpath: browser capture helpers feeding the SDK core (DOM lib).
 * Imported only in browser contexts (spec §8: getUserMedia → AudioWorklet →
 * 16 kHz PCM16). The core ("." entry) stays DOM-free. */
export {
  MicrophonePcm16,
  type MicrophoneCaptureOptions,
  type Pcm16Frame,
} from "./microphone.js";
export {
  RtResampler,
  downmixAndResample,
  type ResamplerOptions,
} from "./resampler.js";
export {
  PCM_WORKLET_NAME,
  PCM_WORKLET_SOURCE,
  attachPcmWorklet,
  createWorkletBlobUrl,
} from "./pcm-worklet.js";
export {
  packPcm16,
  packPcm16FromFloat,
  downmixToMono,
  PCM_SCALE,
} from "./pcm.js";
