import { describe, expect, test } from "bun:test";
import {
  AuthError,
  CancelledError,
  ModelError,
  ProtocolError,
  ServiceError,
  TransportError,
  wrapTransportFailure,
} from "../src/errors.js";
import { closeError } from "../src/session.js";
import { errorFromResponse } from "../src/client.js";
import type { FetchResponseLike } from "../src/transport.js";

function fakeResponse(
  status: number,
  body: string | undefined,
): FetchResponseLike {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: {
      get: () => (status >= 200 && status < 300 ? "application/json" : null),
    },
    text: async () => body ?? "",
    arrayBuffer: async () => new ArrayBuffer(0),
    json: async () => (body ? JSON.parse(body) : undefined),
  };
}

describe("error model", () => {
  test("envelope → ServiceError with status/remoteCode/retryable", async () => {
    const response = fakeResponse(
      503,
      JSON.stringify({
        error: {
          code: "service_busy",
          message: "One live inference lease is already held; retry shortly",
          retryable: true,
          request_id: "r1",
          details: {},
        },
      }),
    );
    const error = await errorFromResponse(response, await response.text());
    expect(error).toBeInstanceOf(ServiceError);
    expect(error?.code).toBe("service");
    expect(error?.status).toBe(503);
    expect(error?.remoteCode).toBe("service_busy");
    expect(error?.retryable).toBe(true);
    expect((error as ServiceError).message).toContain("lease");
  });

  test("model-plane codes map onto ModelError", async () => {
    for (const code of ["model_not_found", "model_not_ready", "prepare-busy"]) {
      const response = fakeResponse(
        409,
        JSON.stringify({
          error: {
            code,
            message: "m",
            retryable: code === "prepare-busy",
            request_id: "r",
          },
        }),
      );
      const error = await errorFromResponse(response, await response.text());
      expect(error).toBeInstanceOf(ModelError);
      expect(error?.remoteCode).toBe(code);
    }
  });

  test("401 empty body → AuthError without remoteCode", async () => {
    const error = await errorFromResponse(fakeResponse(401, ""), "");
    expect(error).toBeInstanceOf(AuthError);
    expect(error?.status).toBe(401);
    expect(error?.retryable).toBe(false);
    expect(error?.remoteCode).toBeUndefined();
  });

  test("2xx returns no error", async () => {
    expect(
      await errorFromResponse(fakeResponse(200, "{}"), "{}"),
    ).toBeUndefined();
  });

  test("fetch throw → TransportError retryable with cause", () => {
    const cause = new Error("ECONNREFUSED");
    const error = wrapTransportFailure(cause);
    expect(error).toBeInstanceOf(TransportError);
    expect(error.retryable).toBe(true);
    expect(error.cause).toBe(cause);
  });

  test("AbortError → CancelledError", () => {
    const cause = Object.assign(new Error("The operation was aborted"), {
      name: "AbortError",
    });
    const error = wrapTransportFailure(cause);
    expect(error).toBeInstanceOf(CancelledError);
  });

  test("close-code carrier table maps onto the six classes", () => {
    expect(closeError(4001, "token rotated").remoteCode).toBe("token_rotated");
    expect(closeError(4001, "r")).toBeInstanceOf(AuthError);
    expect(closeError(4003, "r")).toBeInstanceOf(ServiceError);
    expect(closeError(4003, "r").remoteCode).toBe("stale_resume");
    expect(closeError(1013, "r")).toBeInstanceOf(ServiceError);
    expect(closeError(1011, "r")).toBeInstanceOf(ServiceError);
    expect(closeError(1008, "r")).toBeInstanceOf(ProtocolError);
    expect(closeError(4002, "r")).toBeInstanceOf(ProtocolError);
    expect(closeError(4004, "r")).toBeInstanceOf(ProtocolError);
    expect(closeError(1001, "r")).toBeInstanceOf(TransportError);
    expect(closeError(1001, "r").retryable).toBe(true);
    expect(closeError(4811, "r")).toBeInstanceOf(ProtocolError);
  });

  test("exactly six concrete classes, all tagged", () => {
    const instances = [
      new TransportError({ message: "t" }),
      new AuthError({ message: "a" }),
      new ProtocolError({ message: "p" }),
      new ServiceError({ message: "s" }),
      new ModelError({ message: "m" }),
      new CancelledError({ message: "c" }),
    ];
    for (const e of instances) {
      expect(e).toBeInstanceOf(Error);
      expect(e.code).toBeDefined();
      expect(typeof e.retryable).toBe("boolean");
    }
  });
});
