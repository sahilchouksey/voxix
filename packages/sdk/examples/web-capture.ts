/**
 * Web capture example: map microphone capture onto the SDK core (spec §8 web
 * helper). getUserMedia → AudioWorklet → 16 kHz PCM16 20 ms frames →
 * session.pushAudio → tap. No ScriptProcessorNode; HTTPS/localhost required.
 *
 * Example code lives next to the package source for typechecking; external
 * consumers import "@voxix/sdk" and "@voxix/sdk/web" (the identical exports).
 */
import { VoxixClient } from "../src/client.js";
import { VoxixError } from "../src/errors.js";
import type { VoxixSession } from "../src/session.js";
import { MicrophonePcm16 } from "../src/web/microphone.js";

export interface WebTranscriptionControls {
  /** "user" stop: FIFO barrier + the exactly-one final. */
  stop(): Promise<void>;
  /** Local teardown without a server stop (drop the session). */
  dispose(): void;
}

export function runWebTranscription(opts: {
  baseUrl: string;
  token: string | (() => Promise<string> | string);
  onPartial: (draft: string) => void;
  onFinal: (text: string, historyId: string | number | null) => void;
  onError?: (error: VoxixError) => void;
  device?: MediaTrackConstraints;
}): Promise<WebTranscriptionControls> {
  const client = new VoxixClient({ baseUrl: opts.baseUrl, token: opts.token });
  const mic = new MicrophonePcm16({ chunkMs: 20 });

  return (async () => {
    // 1. Capture first: permission prompt + AudioContext + resampler.
    await mic.start();

    // 2. Create the session, observe the tap BEFORE starting (no lost events).
    const session = await client.createSession({
      vad: { mode: "server" },
      partial_results: true,
      save_history: true,
    });
    void (async () => {
      try {
        for await (const event of session.events) {
          if (event.type === "partial") {
            opts.onPartial(event.payload.committed + event.payload.tentative);
          } else if (event.type === "final") {
            opts.onFinal(event.payload.text, event.payload.history_id ?? null);
          } else if (event.type === "error") {
            opts.onError?.(
              new VoxixError({
                code: "service",
                message: event.payload.message,
                remoteCode: event.payload.code,
                retryable: event.payload.retryable,
              }),
            );
          }
        }
      } catch (error) {
        if (error instanceof VoxixError) opts.onError?.(error);
      }
    })();

    await session.connect();
    await session.start();

    // 3. Push the microphone frames (single consumer; bounded, drop-oldest).
    void (async () => {
      for await (const frame of mic.frames()) {
        try {
          await session.pushAudio(frame.samples, frame.durationMs);
        } catch (error) {
          if (error instanceof VoxixError) opts.onError?.(error);
          break; // the wire rejected; teardown follows via the tap/close
        }
      }
    })();

    return {
      async stop() {
        // Stop the mic first (frames() ends naturally), then the session's
        // FIFO finalize barrier delivers the exactly-one final.
        await mic.stop();
        const final = await session.stop("user");
        opts.onFinal(final.text, final.history_id);
        session.close();
      },
      dispose() {
        void mic.stop().then(() => session.close());
      },
    };
  })();
}
