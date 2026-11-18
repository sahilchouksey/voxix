/**
 * Browser capture → SDK core mapping (spec §8 web helper): getUserMedia →
 * AudioWorklet (real device rate, downmixed mono) → RtResampler → 16 kHz
 * PCM16 frames feeding `session.pushAudio`. No ScriptProcessorNode anywhere;
 * mic capture requires HTTPS/localhost.
 *
 * The internal queue is bounded with drop-oldest (a slow consumer sheds
 * audio instead of growing unboundedly); each transition carries the same
 * `durationMs` so frames map 1:1 onto session pushes.
 */
import { RtResampler } from "./resampler.js";
import { attachPcmWorklet, PCM_WORKLET_NAME } from "./pcm-worklet.js";

export type Pcm16Frame = {
  samples: Int16Array;
  durationMs: 10 | 20 | 30 | 40 | 60;
  /** Exactly the service profile: the pushes decode as i16/32768. */
  codec: "pcm_s16le";
  sampleRateHz: 16000;
  channels: 1;
};

export interface MicrophoneCaptureOptions {
  chunkMs?: 10 | 20 | 30 | 40 | 60;
  echoCancellation?: boolean;
  noiseSuppression?: boolean;
  autoGainControl?: boolean;
}

const QUEUE_FRAME_CAP = 64; // ~1.28 s at 20 ms — memory bound, drop-oldest

/** One microphone capture session; consume `frames` once and stop it. */
export class MicrophonePcm16 {
  private readonly chunkMs: 10 | 20 | 30 | 40 | 60;
  private readonly trackConstraints:
    | Omit<MediaTrackConstraints, "deviceId">
    | boolean
    | undefined;
  private stream: MediaStream | undefined;
  private context: AudioContext | undefined;
  private node: AudioWorkletNode | undefined;
  private source: MediaStreamAudioSourceNode | undefined;
  private resampler: RtResampler | undefined;
  private queue: Pcm16Frame[] = [];
  private queueDropped = 0;
  private wake: (() => void) | undefined;
  private closed = false;
  private framesEmittedCount = 0;
  private running = false;

  constructor(options: MicrophoneCaptureOptions = {}) {
    this.chunkMs = options.chunkMs ?? 20;
    this.trackConstraints = {
      echoCancellation: options.echoCancellation ?? true,
      noiseSuppression: options.noiseSuppression ?? true,
      autoGainControl: options.autoGainControl ?? true,
    };
  }

  get framesEmitted(): number {
    return this.framesEmittedCount;
  }

  get dropped(): number {
    return this.queueDropped;
  }

  /** Mic permission + AudioContext (device rate) + worklet wiring. */
  async start(): Promise<void> {
    if (this.running)
      throw new Error("the microphone capture is already running");
    if (
      typeof navigator === "undefined" ||
      !navigator.mediaDevices?.getUserMedia
    ) {
      throw new Error(
        "getUserMedia is unavailable — capture requires HTTPS/localhost",
      );
    }
    this.running = true;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: this.trackConstraints,
        video: false,
      });
      this.stream = stream;
      // SampleRate omitted: the context runs at the DEVICE rate; the
      // resampler normalizes to 16 kHz (never trust a fixed capture rate).
      const context = new AudioContext();
      this.context = context;
      await context.resume();
      this.resampler = new RtResampler({
        sourceRateHz: context.sampleRate,
        targetRateHz: 16_000,
        chunkMs: this.chunkMs,
      });
      const node = await attachPcmWorklet(context, (mono) => this.onMono(mono));
      this.node = node;
      this.source = context.createMediaStreamSource(stream);
      this.source.connect(node);
    } catch (cause) {
      await this.stop();
      throw cause;
    }
  }

  /**
   * Single-consumer async generator of fixed-chunk PCM16 frames (each one
   * maps 1:1 onto `session.pushAudio`). Bounded: the newest audio wins,
   * stale frames are dropped when the consumer lags.
   */
  async *frames(): AsyncGenerator<Pcm16Frame, void, unknown> {
    if (!this.node || !this.running) {
      throw new Error("start() the capture before frames()");
    }
    try {
      while (!this.closed) {
        while (this.queue.length > 0 && !this.closed) {
          const frame = this.queue.shift();
          if (!frame) break;
          this.framesEmittedCount++;
          yield frame;
        }
        if (this.closed) break;
        await new Promise<void>((resolve) => {
          this.wake = resolve;
          if (this.closed) resolve();
        });
        this.wake = undefined;
      }
    } finally {
      this.queue.length = 0;
    }
  }

  async stop(): Promise<void> {
    this.closed = true;
    try {
      if (this.source) {
        try {
          this.source.disconnect();
        } catch {
          /* already disconnected */
        }
      }
      if (this.node) {
        try {
          this.node.port.postMessage({ stream: false });
          this.node.disconnect();
        } catch {
          /* already disconnected */
        }
      }
      if (this.context && this.context.state !== "closed") {
        await this.context.close();
      }
    } finally {
      this.stream?.getTracks().forEach((track) => track.stop());
      this.stream = undefined;
      this.node = undefined;
      this.source = undefined;
      this.context = undefined;
      this.resampler = undefined;
      this.running = false;
      const wake = this.wake;
      this.wake = undefined;
      wake?.();
    }
  }

  private onMono(mono: Float32Array): void {
    if (!this.resampler) return;
    for (const samples of this.resampler.push(mono)) {
      if (this.queue.length >= QUEUE_FRAME_CAP) {
        this.queue.shift();
        this.queueDropped++;
      }
      this.queue.push({
        samples,
        durationMs: this.chunkMs,
        codec: "pcm_s16le",
        sampleRateHz: 16000,
        channels: 1,
      });
      const wake = this.wake;
      this.wake = undefined;
      wake?.();
    }
  }
}

export { PCM_WORKLET_NAME };
