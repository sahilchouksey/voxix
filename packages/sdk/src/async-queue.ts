/**
 * The observation tap (spec §15 G25, SDK plan §8): a bounded, drop-oldest,
 * single-consumer AsyncIterable. A second concurrent registration throws the
 * strict-consumer error instead of silently interleaving two readers.
 */
import { ProtocolError } from "./errors.js";

export const STRICT_CONSUMER_MESSAGE =
  "strict-consumer: this tap is already owned by an active AsyncIterable consumer";

export class BoundedTap<T> {
  private buffer: T[] = [];
  private readonly capacity: number;
  private closed = false;
  private failureCause: unknown = undefined;
  private droppedTotal = 0;
  private owned = false;
  private waiters: Array<{
    resolve(signal: { done: true } | { done: false }): void;
    reject(cause: unknown): void;
  }> = [];

  constructor(capacity = 512) {
    if (!Number.isInteger(capacity) || capacity < 1) {
      throw new Error(`tap capacity must be a positive integer, got ${capacity}`);
    }
    this.capacity = capacity;
  }

  get dropped(): number {
    return this.droppedTotal;
  }

  get size(): number {
    return this.buffer.length;
  }

  /** Enqueue one value; the oldest buffered value is dropped at capacity. */
  push(value: T): void {
    if (this.closed) return;
    if (this.buffer.length >= this.capacity) {
      this.buffer.shift();
      this.droppedTotal++;
    }
    this.buffer.push(value);
    const waiter = this.waiters.shift();
    if (waiter) waiter.resolve({ done: false });
  }

  /** Close normally: pending and future iterations drain then end. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    const waiters = this.waiters.splice(0);
    for (const waiter of waiters) waiter.resolve({ done: true });
  }

  /** Close with a terminal failure. Buffered values stay observable first
   * (next() drains the buffer before consulting the failure), then
   * iterations reject with the cause. */
  fail(cause: unknown): void {
    if (this.closed) return;
    this.closed = true;
    this.failureCause = cause;
    const waiters = this.waiters.splice(0);
    for (const waiter of waiters) waiter.reject(cause);
  }

  /**
   * Register the (single) consumer. A second registration — concurrent, or
   * after an earlier consumer broke out — errors strictly: the tap never
   * silently interleaves, replays, or re-registers after a partial read.
   */
  [Symbol.asyncIterator](): AsyncIterator<T> {
    if (this.owned) {
      throw new ProtocolError({ message: STRICT_CONSUMER_MESSAGE });
    }
    this.owned = true;
    const tap = this;
    const iterator: AsyncIterator<T> = {
      next(): Promise<IteratorResult<T>> {
        if (tap.buffer.length > 0) {
          return Promise.resolve({ done: false, value: tap.buffer.shift() as T });
        }
        if (tap.failureCause !== undefined) return Promise.reject(tap.failureCause);
        if (tap.closed) return Promise.resolve({ done: true, value: undefined as never });
        return new Promise<IteratorResult<T>>((resolve, reject) => {
          tap.waiters.push({
            resolve: (signal) => {
              if (signal.done) resolve({ done: true, value: undefined as never });
              else resolve({ done: false, value: tap.buffer.shift() as T });
            },
            reject,
          });
        });
      },
      return(): Promise<IteratorResult<T>> {
        // The consumer abandoned the tap: ownership stays taken (a later
        // registration errors strictly), and leftover values are dropped.
        tap.buffer.length = 0;
        tap.closed = true;
        const waiters = tap.waiters.splice(0);
        for (const waiter of waiters) waiter.resolve({ done: true });
        return Promise.resolve({ done: true, value: undefined as never });
      },
    };
    return iterator;
  }
}
