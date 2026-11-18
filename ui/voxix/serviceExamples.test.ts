import { describe, expect, test } from "bun:test";
import { curlExample, jsExample, pythonExample } from "./serviceExamples";

describe("service examples", () => {
  test("curlExample embeds the given token as the bearer credential", () => {
    expect(curlExample("vx_abc123").includes("Bearer vx_abc123")).toBe(true);
  });

  test("jsExample embeds the given token", () => {
    expect(jsExample("vx_abc123").includes("vx_abc123")).toBe(true);
  });

  test("pythonExample embeds the given token", () => {
    expect(pythonExample("vx_abc123").includes("vx_abc123")).toBe(true);
  });

  test("default arguments produce the vx_YOUR_TOKEN placeholder", () => {
    expect(curlExample().includes("vx_YOUR_TOKEN")).toBe(true);
    expect(jsExample().includes("vx_YOUR_TOKEN")).toBe(true);
    expect(pythonExample().includes("vx_YOUR_TOKEN")).toBe(true);
  });
});
