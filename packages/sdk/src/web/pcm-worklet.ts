/**
 * The AudioWorklet processor serializer (see microphone.ts for the consumer).
 * Worklets load from a module URL; this module carries the worklet's source
 * verbatim so apps may inline it via a Blob URL (no extra served file).
 *
 * Main-thread → worklet messages:
 *   { stream: false } — stop feeding (recording torn down)
 * Worklet → main-thread messages:
 *   { type: "pcm", mono: Float32Array } — downmixed mono input at the
 *   AUDIO CONTEXT's real sample rate (never assume 48 kHz).
 */
export const PCM_WORKLET_NAME = "voxix-pcm-capture";

export const PCM_WORKLET_SOURCE = `
class VxPcmCapture {
  streamOn = true;
  constructor() {
    this.port.onmessage = (e) => {
      const data = e.data || {};
      if (typeof data.stream === "boolean") this.streamOn = data.stream;
    };
  }
  process(inputs) {
    const input = inputs[0];
    if (this.streamOn && input && input.length > 0) {
      const active = input.filter((ch) => ch && ch.length > 0);
      if (active.length > 0) {
        const frames = active[0].length;
        const mono = new Float32Array(frames);
        for (let i = 0; i < frames; i++) {
          let sum = 0;
          for (let c = 0; c < active.length; c++) sum += active[c][i];
          mono[i] = sum / active.length;
        }
        this.port.postMessage({ type: "pcm", mono });
      }
    }
    return true;
  }
}
registerProcessor(${JSON.stringify(PCM_WORKLET_NAME)}, VxPcmCapture);
`;

/** Blob URL carrying the worklet module (inline, no extra fetch). */
export function createWorkletBlobUrl(): string {
  const blob = new Blob([PCM_WORKLET_SOURCE], { type: "application/javascript" });
  return URL.createObjectURL(blob);
}

/** Register + attach the capture worklet to a context. */
export async function attachPcmWorklet(
  context: AudioContext,
  onPcm: (mono: Float32Array) => void,
  moduleUrl?: string,
): Promise<AudioWorkletNode> {
  await context.audioWorklet.addModule(moduleUrl ?? createWorkletBlobUrl());
  const node = new AudioWorkletNode(context, PCM_WORKLET_NAME, {
    numberOfInputs: 1,
    numberOfOutputs: 0,
  });
  node.port.onmessage = (event) => {
    const data = event.data as { type: string; mono?: Float32Array };
    if (data?.type === "pcm" && data.mono instanceof Float32Array) onPcm(data.mono);
  };
  return node;
}
