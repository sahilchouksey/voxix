import { describe, expect, test } from "bun:test";
import {
  composeTokens,
  isModifierOnly,
  parseBinding,
  tokenFor,
} from "./hotkeyTokens";

describe("tokenFor", () => {
  test("Option+Space is space even when macOS reports a non-breaking space", () => {
    expect(tokenFor({ key: "\u00A0", code: "Space" })).toBe("space");
  });

  test("modifier keys are resolved from their physical side", () => {
    expect(tokenFor({ key: "Alt", code: "AltLeft" })).toBe("alt");
    expect(tokenFor({ key: "Meta", code: "MetaRight" })).toBe("super");
  });

  test("Option-modified letters resolve to the physical key", () => {
    // macOS reports "å" for Option+A; the binding must still be "a".
    expect(tokenFor({ key: "å", code: "KeyA" })).toBe("a");
  });

  test("digits, function keys and arrows", () => {
    expect(tokenFor({ key: "1", code: "Digit1" })).toBe("1");
    expect(tokenFor({ key: "F5", code: "F5" })).toBe("f5");
    expect(tokenFor({ key: "ArrowLeft", code: "ArrowLeft" })).toBe("left");
  });

  test("falls back to key when no code is available", () => {
    expect(tokenFor({ key: " " })).toBe("space");
    expect(tokenFor({ key: "K" })).toBe("k");
  });
});

describe("composeTokens", () => {
  test("builds the canonical Option+Space binding", () => {
    expect(composeTokens(["alt", "space"])).toEqual(["alt", "space"]);
    expect(composeTokens(["alt", "space"]).join("+")).toBe("alt+space");
  });

  test("orders modifiers before the main key and keeps only the last key", () => {
    expect(composeTokens(["space", "shift", "alt"])).toEqual([
      "alt",
      "shift",
      "space",
    ]);
    expect(composeTokens(["ctrl", "a", "b"])).toEqual(["ctrl", "b"]);
  });

  test("modifier-only chords stay modifier-only", () => {
    const tokens = composeTokens(["alt", "shift"]);
    expect(tokens).toEqual(["alt", "shift"]);
    expect(tokens.every((t) => ["ctrl", "alt", "shift", "super"].includes(t))).toBe(true);
  });
});

describe("parseBinding / isModifierOnly", () => {
  test("parses stored bindings case-insensitively", () => {
    expect(parseBinding("alt+Space")).toEqual(["alt", "space"]);
    expect(parseBinding("Ctrl+Shift+A")).toEqual(["ctrl", "shift", "a"]);
  });

  test("recognises modifier-only bindings", () => {
    expect(isModifierOnly("alt+shift")).toBe(true);
    expect(isModifierOnly("alt+space")).toBe(false);
    expect(isModifierOnly("")).toBe(false);
  });
});
