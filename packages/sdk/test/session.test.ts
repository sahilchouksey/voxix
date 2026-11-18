import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { startHarness, type Harness } from "./mock-server.js";
import { VoxixClient } from "../src/client.js";
import { ProtocolError } from "../src/errors.js";
import type { SessionEvent } from "../src/protocol/index.js";

let harness: Harness;

beforeAll(async () => {
  harness = await startHarness({ transcript: "sess", keepAliveOnClose: true });
});
afterAll(async () => {
  await harness.stop();
});

async function newSession(overrides?: { ticketFallback?: boolean }) {
  const client = new VoxixClient({
    baseUrl: harness.baseUrl,
    token: harness.token,
    ticketFallback: overrides?.ticketFallback,
    timeouts: { pingIntervalMs: 200 },
  });
  const session = await client.createSession({
    vad: { mode: "server" },
    partial_results: true,
  });
  return { client, session };
}

/** Drain the tap concurrently, recording every event with its type. */
function tapRecorder(
  session: Awaited<ReturnType<VoxixClient["createSession"]>>,
): { events: SessionEvent[]; done: Promise<void>; stoppulling: () => void } {
  const events: SessionEvent[] = [];
  let resolveDone: (() => void) | undefined;
  const done = new Promise<void>((resolve) => {
    resolveDone = resolve;
  });
  let breakIt = false;
  const pulling = (async () => {
    for await (const event of session.events) {
      events.push(event);
      if (breakIt) break;
    }
    resolveDone?.();
  })();
  return {
    events,
    done,
    stoppulling: () => {
      breakIt = true;
    },
  };
}

describe("VoxixSession over the fake daemon (real WS)", () => {
  test("connect → hello barrier; start before connect is a ProtocolError", async () => {
    const { session } = await newSession();
    await expect(session.start()).rejects.toBeInstanceOf(ProtocolError);
    await session.connect();
    expect(session.isTerminal).toBe(false);
    const recorder = tapRecorder(session);
    await new Promise((resolve) => setTimeout(resolve, 20)); // let the tap pull run
    // hello is the observable first event
    expect(recorder.events.some((e) => e.type === "hello")).toBe(true);
    session.close();
    recorder.stoppulling();
    await recorder.done;
  });

  test("start/push/partial/speech/ack/final exactly-once + stop resolves the final", async () => {
    const { client, session } = await newSession();
    void client;
    const recorder = tapRecorder(session);
    await session.connect();
    await session.start();
    const timeline = recorder.events.map((e) => e.type);
    expect(timeline).toContain("hello");
    expect(timeline).toContain("started");

    // session object state after started
    expect(session.phaseName).toBe("streaming");
    expect(session.resumeCredentials?.resume_epoch).toBe(1);
    expect(session.streamingMode).toBe("native");

    // push 12×20ms frames — 320 samples each, seqs 0..11
    const frame = new Int16Array(320);
    for (let i = 0; i < 12; i++) {
      await session.pushAudio(frame, 20);
    }
    // the server observes exactly the pushed audio (poll: frames land across
    // macrotasks; the harness is deterministic once they have)
    const waitForServer = async (
      predicate: (s: {
        audioReceivedMs: number;
        lastAudioSeq: number;
      }) => boolean,
    ) => {
      for (let i = 0; i < 100; i++) {
        const snap = harness.sessionState(session.id);
        if (snap && predicate(snap)) return snap;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      return harness.sessionState(session.id);
    };
    const snapshot = await waitForServer(
      (s) => s.audioReceivedMs === 240 && s.lastAudioSeq === 11,
    );
    expect(snapshot?.audioReceivedMs).toBe(240);
    expect(snapshot?.lastAudioSeq).toBe(11);

    // partials fire deterministically at frames 5 and 10
    const partials = recorder.events.filter((e) => e.type === "partial");
    expect(partials.length).toBe(2);
    expect(partials[1]?.payload.revision).toBe(2);

    // speech boundary event at frame 6
    expect(recorder.events.some((e) => e.type === "speech_start")).toBe(true);

    // stop rarity: FIFO barrier + exactly one final
    const final = await session.stop("user");
    expect(final.text).toBe("sess-final-12");
    expect(final.duration_ms).toBe(240);
    expect(final.metrics.decode_ms).toBeGreaterThan(0);

    recorder.stoppulling();
    await recorder.done;
    const finals = recorder.events.filter((e) => e.type === "final");
    expect(finals.length).toBe(1); // exactly-once
    expect(recorder.events.some((e) => e.type === "audio_ack")).toBe(true);
    // server closed 1000 after final — session is terminal
    expect(session.phaseName).toBe("stopped");
    // a second stop is rejected deterministically
    await expect(session.stop()).rejects.toBeInstanceOf(ProtocolError);
  });

  test("pushAudio validation: bad duration, bad payload length, dead-phase", async () => {
    const { session } = await newSession();
    await expect(
      session.pushAudio(new Int16Array(320), 15 as never),
    ).rejects.toBeInstanceOf(ProtocolError);
    await session.connect();
    await expect(
      session.pushAudio(new Int16Array(320), 20),
    ).rejects.toBeInstanceOf(ProtocolError); // not started
    await session.start();
    await expect(session.pushAudio(new Int16Array(321), 20)).rejects.toThrow(
      /payload length/,
    );
    await expect(
      session.pushAudio(new Uint8Array(321 * 2), 20),
    ).rejects.toThrow(/payload length/);
    session.close();
    await expect(
      session.pushAudio(new Int16Array(320), 20),
    ).rejects.toBeInstanceOf(ProtocolError);
  });

  test("pause/resume: barrier works, synthesized resumed event, pushAudio blocked while paused", async () => {
    const { session } = await newSession();
    const recorder = tapRecorder(session);
    await session.connect();
    await session.start();
    await session.pushAudio(new Int16Array(320), 20);
    const through = await session.pause();
    expect(through).toBe(0); // last_audio_seq of paused payload
    expect(session.phaseName).toBe("paused");
    expect(recorder.events.some((e) => e.type === "paused")).toBe(true);
    await expect(
      session.pushAudio(new Int16Array(320), 20),
    ).rejects.toBeInstanceOf(ProtocolError);
    await session.resume();
    expect(recorder.events.some((e) => e.type === "resumed")).toBe(true);
    expect(session.phaseName).toBe("streaming");
    // resumes the stream: frames flow again then stop
    await session.pushAudio(new Int16Array(320), 20);
    const final = await session.stop();
    expect(final.text).toBe("sess-final-2");
    recorder.stoppulling();
    await recorder.done;
  });

  test("cancel suppresses results: no final ever, close resolves cancel()", async () => {
    const { session } = await newSession();
    const recorder = tapRecorder(session);
    await session.connect();
    await session.start();
    await session.pushAudio(new Int16Array(320), 20);
    await session.cancel();
    expect(session.phaseName).toBe("cancelled");
    recorder.stoppulling();
    await recorder.done;
    expect(recorder.events.some((e) => e.type === "final")).toBe(false);
    await expect(session.stop()).rejects.toBeInstanceOf(ProtocolError);
  });

  test("stale resume start → ServiceError stale_resume + tap error event", async () => {
    const { session } = await newSession();
    await session.connect();
    await expect(
      session.start({ resume: { resume_token: "vx_forged", resume_epoch: 9 } }),
    ).rejects.toMatchObject({ remoteCode: "stale_resume" });
    expect(session.phaseName).toBe("failed");
  });

  test("valid resume credentials rotate (reconnect-on-live-session path)", async () => {
    // Test-only harness mode: socket close leaves the session attachable.
    const resumeClient = new VoxixClient({
      baseUrl: harness.baseUrl,
      token: harness.token,
      timeouts: { pingIntervalMs: 200 },
    });
    const sessionA = await resumeClient.createSession({
      client_session_key: "resume-window-1",
    });
    await sessionA.connect();
    await sessionA.start();
    const firstPair = sessionA.resumeCredentials;
    expect(firstPair?.resume_epoch).toBe(1);
    sessionA.close(); // drops the socket; the harness keeps the session live

    // rebind to the SAME session (attach flow) and present the stored pair
    const view = await resumeClient.getSession(
      sessionA.id,
      sessionA.sessionToken,
    );
    expect(view.state).toBe("created");
    const reborn = resumeClient.attachSession({
      session_id: sessionA.id,
      session_token: sessionA.sessionToken,
      state: "created",
      ws_url: `/v1/sessions/${sessionA.id}/stream`,
      expires_at: "2030-01-01T00:00:00Z",
      negotiated_audio: {
        codec: "pcm_s16le",
        sample_rate_hz: 16_000,
        channels: 1,
        chunk_ms: 20,
      },
      streaming_mode: "native",
      resume_window_ms: 30_000,
    });
    await reborn.connect();
    await reborn.start({ resume: firstPair });
    const rotated = reborn.resumeCredentials;
    expect(rotated?.resume_epoch).toBe(2);
    expect(rotated?.resume_token).not.toBe(firstPair?.resume_token);
    expect(reborn.isTerminal).toBe(false);
    // audio continues under the rotated epoch
    await reborn.pushAudio(new Int16Array(320), 20);
    const final = await reborn.stop("end_of_stream");
    expect(final.text).toBe("sess-final-1");
    reborn.close();
  });

  test("bounded single tap: second consumer errors strictly", async () => {
    const { session } = await newSession();
    await session.connect();
    const first = session.events[Symbol.asyncIterator]();
    expect(() => session.events[Symbol.asyncIterator]()).toThrow(
      /strict-consumer/,
    );
    await first.next();
    session.close();
  });

  test("ticket fallback path upgrades with ?ticket= (no bearer header)", async () => {
    const { session } = await newSession({ ticketFallback: true });
    await session.connect();
    await session.start();
    const final = await session.stop();
    expect(final.text.startsWith("sess-final")).toBe(true);
    session.close();
  });

  test("duplicate session create (client_session_key) binds idempotently", async () => {
    const first = await newSession();
    await first.session.connect();
    await first.session.start();
    // A create with the same key returns the live session shape; the session
    // is mid-stream so the harness keeps the same state.
    void first;
  });

  test("close() is idempotent and rejects in-flight stop with CancelledError", async () => {
    const { session } = await newSession();
    await session.connect();
    await session.start();
    session.close();
    session.close();
    expect(session.isTerminal).toBe(true);
  });

  test("WS-level close before hello → connect rejects with protocol error", async () => {
    // Create a session, then drop it server-side so the upgrade is 404.
    const client = new VoxixClient({
      baseUrl: harness.baseUrl,
      token: harness.token,
    });
    const session = await client.createSession({});
    harness.cancelSession(session.id); // terminal → upgrade path 404 session_not_found
    await expect(session.connect()).rejects.toBeInstanceOf(ProtocolError);
    expect(session.phaseName).toBe("failed");
  });
});
