import { describe, expect, test } from "bun:test";
import { BoundedTap, STRICT_CONSUMER_MESSAGE } from "../src/async-queue.js";
import { ProtocolError } from "../src/errors.js";

describe("bounded drop-oldest single-consumer tap (§15 G25)", () => {
  test("delivers values in order; drop-oldest at capacity", async () => {
    const tap = new BoundedTap<number>(3);
    for (const value of [1, 2, 3, 4, 5]) tap.push(value);
    expect(tap.dropped).toBe(2);
    expect(tap.size).toBe(3);
    tap.close(); // stop producing; iteration drains then ends
    const seen: number[] = [];
    for await (const value of tap) seen.push(value);
    expect(seen).toEqual([3, 4, 5]);
  });

  test("close() ends iteration after draining", async () => {
    const tap = new BoundedTap<string>();
    tap.push("a");
    tap.push("b");
    tap.close();
    const seen: string[] = [];
    for await (const value of tap) seen.push(value);
    expect(seen).toEqual(["a", "b"]);
  });

  test("fail() delivers buffered values before rejecting", async () => {
    const tap = new BoundedTap<string>();
    tap.push("a");
    tap.fail(new Error("transport gone"));
    const seen: string[] = [];
    await (async () => {
      for await (const value of tap) seen.push(value);
    })().catch(() => undefined);
    expect(seen).toEqual(["a"]);
  });

  test("second concurrent consumer errors strictly", () => {
    const tap = new BoundedTap<number>();
    const first = tap[Symbol.asyncIterator]();
    expect(() => tap[Symbol.asyncIterator]()).toThrow(ProtocolError);
    let message = "";
    try {
      tap[Symbol.asyncIterator]();
    } catch (e) {
      message = (e as Error).message;
    }
    expect(message).toContain("strict-consumer");
    expect(message).toContain(STRICT_CONSUMER_MESSAGE.slice(0, 20));
    // The first consumer still works.
    tap.push(1);
    return expect(first.next()).resolves.toEqual({ done: false, value: 1 });
  });

  test("ownership persists after an early break (no re-registration)", async () => {
    const tap = new BoundedTap<number>();
    tap.push(1);
    for await (const _value of tap) break; // abandon mid-stream
    tap.push(2);
    expect(() => tap[Symbol.asyncIterator]()).toThrow(ProtocolError);
  });

  test("pending waiters resolve on push and close", async () => {
    const tap = new BoundedTap<number>();
    const iterator = tap[Symbol.asyncIterator]();
    const pending = iterator.next();
    tap.push(42);
    expect(await pending).toEqual({ done: false, value: 42 });
    const closePending = iterator.next();
    tap.close();
    expect(await closePending).toEqual({ done: true });
  });

  test("small capacity is enforced as a positive integer", () => {
    expect(() => new BoundedTap(0)).toThrow(/capacity/);
  });
});
