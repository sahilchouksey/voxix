import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { startHarness, type Harness } from "./mock-server.js";
import { VoxixClient } from "../src/client.js";
import {
  AuthError,
  ModelError,
  ProtocolError,
  ServiceError,
  TransportError,
} from "../src/errors.js";
import type { FetchLike } from "../src/transport.js";

let harness: Harness;

beforeAll(async () => {
  harness = await startHarness({ label: "client", transcript: "client-test" });
});
afterAll(async () => {
  await harness.stop();
});

function authed(): VoxixClient {
  return new VoxixClient({ baseUrl: harness.baseUrl, token: harness.token });
}

function wavBytes(): ArrayBuffer {
  const sampleBytes = 320 * 2;
  const buffer = new ArrayBuffer(44 + sampleBytes);
  const view = new DataView(buffer);
  const text = (offset: number, value: string) => {
    for (let i = 0; i < value.length; i++)
      view.setUint8(offset + i, value.charCodeAt(i));
  };
  text(0, "RIFF");
  view.setUint32(4, 36 + sampleBytes, true);
  text(8, "WAVE");
  text(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // pcm
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, 16000, true);
  view.setUint32(28, 16000 * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, sampleBytes, true);
  return buffer;
}

describe("VoxixClient over the fake daemon (real HTTP)", () => {
  test("alive/health/status", async () => {
    const client = authed();
    expect(await client.alive()).toBe(true);
    const health = await client.health();
    expect(health.protocol_version).toBe(1);
    expect(health.engine_version).toContain("0.1.0");
    const status = await client.status();
    expect(status.capabilities.max_concurrent_streams).toBe(1);
    expect(status.active_model_id).toBe("tiny-16k");
  });

  test("token provider fn resolves per call; bearer scheme added", async () => {
    let calls = 0;
    const client = new VoxixClient({
      baseUrl: harness.baseUrl,
      token: () => {
        calls += 1;
        return harness.token;
      },
    });
    await client.health();
    expect(calls).toBeGreaterThanOrEqual(1);
  });

  test("unauthed requests answer 401 empty body → AuthError", async () => {
    const client = new VoxixClient({
      baseUrl: harness.baseUrl,
      token: "vx_wrong_token",
    });
    await expect(client.health()).rejects.toBeInstanceOf(AuthError);
  });

  test("listModels/select/getModel/getDownload", async () => {
    const client = authed();
    const page = await client.listModels({ installed: true });
    expect(page.models.map((m) => m.id)).toContain("tiny-16k");
    expect(page.selected_model_id).toBe("tiny-16k");

    const selection = await client.selectModel("tiny-16k", "lazy");
    expect(selection.load).toBe("lazy");
    expect(selection.model_id).toBe("tiny-16k");

    const single = await client.getModel("tiny-16k");
    expect(single.prepare.stage).toBe("ok");

    const download = await client.getDownload("tiny-16k");
    expect(download.total_bytes).toBe(75_000_000);
  });

  test("prepare SSE → prepareModel resolves ok with progress stages", async () => {
    const client = authed();
    const stages: string[] = [];
    const result = await client.prepareModel("tiny-16k", (event) =>
      stages.push(event.event),
    );
    expect(stages).toEqual(["verifying", "loading", "ok"]);
    expect(result.state).toBe("completed");
    expect(result.backend).toBe("transcribe_cpp");
  });

  test("prepare SSE terminal error → ModelError", async () => {
    const client = authed();
    expect(client.prepareModel("missing-model")).rejects.toBeInstanceOf(
      ModelError,
    );
  });

  test("prepare-busy is a ModelError (held slot)", async () => {
    const client = authed();
    const release = harness.holdPrepare();
    try {
      await expect(client.prepareModel("tiny-16k")).rejects.toBeInstanceOf(
        ModelError,
      );
    } finally {
      release();
    }
  });

  test("downloadModel/cancelModelDownload managed-download round trip", async () => {
    const client = authed();
    // Already installed → 200 with the completed projection.
    const completed = await client.downloadModel("tiny-16k");
    expect(completed.state).toBe("completed");
    expect(completed.total_bytes).toBe(75_000_000);

    // Not installed → 202 starts the single in-flight download.
    const started = await client.downloadModel("catalog-large");
    expect(started.state).toBe("downloading");
    // Idempotent: a repeated POST converges on the same download.
    const converged = await client.downloadModel("catalog-large");
    expect(converged.state).toBe("downloading");
    // Progress poll reflects it.
    const polled = await client.getDownload("catalog-large");
    expect(polled.state).toBe("downloading");

    // Cancel → resolves (204); a second cancel reports nothing in flight (422).
    await client.cancelModelDownload("catalog-large");
    await expect(
      client.cancelModelDownload("catalog-large"),
    ).rejects.toBeInstanceOf(ServiceError);

    // Unknown id → ModelError (404 model_not_found).
    await expect(client.downloadModel("no-such-model")).rejects.toBeInstanceOf(
      ModelError,
    );
    await expect(
      client.cancelModelDownload("no-such-model"),
    ).rejects.toBeInstanceOf(ModelError);
  });

  test("batch transcribeFile → 202 → poll completed → history row", async () => {
    const client = authed();
    const job = await client.transcribeFile(wavBytes());
    expect(job.state).toBe("processing");
    expect(job.status_url).toBe(`/v1/transcriptions/${job.job_id}`);
    // deterministic decode tail completes within a tick
    await new Promise((r) => setTimeout(r, 30));
    const done = await client.getTranscription(job.job_id);
    expect(done.state).toBe("completed");
    if (done.state === "completed") {
      expect(done.result?.transcript).toBe("batch-fixture-transcript");
    }
    const page = await client.history({ limit: 5 });
    expect(page.entries.length).toBeGreaterThan(0);
    expect(page.has_more).toBe(false);
  });

  test("config GET/PATCH round-trip; unknown fields → 422 ServiceError", async () => {
    const client = authed();
    const config = await client.config();
    expect(config.max_sessions).toBe(4);
    const patched = await client.patchConfig({ max_audio_seconds: 300 });
    expect(patched.max_audio_seconds).toBe(300);
    await expect(
      client.patchConfig({ unknown_field: 1 } as never),
    ).rejects.toBeInstanceOf(ServiceError);
    await client.patchConfig({ max_audio_seconds: 120 });
  });

  test("session create + REST projection + wrong sub-token → ServiceError 404", async () => {
    const client = authed();
    const session = await client.createSession({ vad: { mode: "server" } });
    expect(session.id).toBeDefined();
    expect(session.streamingMode).toBe("native");
    expect(session.negotiatedAudio.sample_rate_hz).toBe(16_000);

    const view = await client.getSession(session.id, session.sessionToken);
    expect(view.state).toBe("created");

    await expect(
      client.getSession(session.id, "vx_mississippi"),
    ).rejects.toMatchObject({
      remoteCode: "session_not_found",
      status: 404,
    });
  });

  test("injected fetch override is honored (URL join + bearer + envelope mapping)", async () => {
    const calls: Array<{
      url: string;
      method?: string;
      headers?: Record<string, string>;
    }> = [];
    const fake: FetchLike = async (url, init) => {
      calls.push({ url, method: init?.method, headers: init?.headers });
      if (url.endsWith("/v1/health")) {
        return {
          ok: true,
          status: 200,
          headers: { get: () => "application/json" },
          text: async () =>
            JSON.stringify({
              status: "ok",
              version: "x",
              engine_version: "y",
              protocol_version: 1,
              uptime_ms: 0,
              ready: true,
            }),
          arrayBuffer: async () => new ArrayBuffer(0),
          json: async () => ({
            status: "ok",
            version: "x",
            engine_version: "y",
            protocol_version: 1,
            uptime_ms: 0,
            ready: true,
          }),
        };
      }
      return {
        ok: false,
        status: 429,
        headers: { get: () => "application/json" },
        text: async () =>
          JSON.stringify({
            error: {
              code: "queue-full",
              message: "capacity",
              retryable: true,
              request_id: "r",
            },
          }),
        arrayBuffer: async () => new ArrayBuffer(0),
        json: async () => ({
          error: {
            code: "queue-full",
            message: "capacity",
            retryable: true,
            request_id: "r",
          },
        }),
      };
    };
    const client = new VoxixClient({
      baseUrl: "http://vxix.local:47832/",
      token: "vx_t",
      fetch: fake,
    });
    const health = await client.health();
    expect(health.protocol_version).toBe(1);
    expect(calls[0]?.url).toBe("http://vxix.local:47832/v1/health");
    expect(calls[0]?.headers?.Authorization).toBe("Bearer vx_t");
    expect(client.listModels()).rejects.toMatchObject({
      remoteCode: "queue-full",
      status: 429,
    });
  });

  test("transport failure (dead server) → TransportError retryable", async () => {
    const client = new VoxixClient({
      baseUrl: "http://127.0.0.1:1",
      token: "vx_t",
    });
    await expect(client.health()).rejects.toBeInstanceOf(TransportError);
  });

  test("protocol misuse inputs throw ProtocolError", async () => {
    expect(() => new VoxixClient({ baseUrl: "", token: "x" })).toThrow(
      ProtocolError,
    );
  });
});
