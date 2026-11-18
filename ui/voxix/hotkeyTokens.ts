/**
 * Pure helpers for the shortcut recorder.
 *
 * The recorder must produce the same token format that the Rust
 * `global-hotkey` parser accepts (`alt+space`, `ctrl+shift+a`, ...). Two
 * macOS-specific pitfalls are handled here:
 *
 * 1. `KeyboardEvent.code` is layout- and modifier-independent, while `key`
 *    changes when modifiers are held. Option+Space, for example, reports
 *    `key === "\u00A0"` (non-breaking space) instead of `" "`, which used to
 *    be saved as an unparseable binding.
 * 2. Modifier keys must be recognised from their physical side codes.
 */

const GLYPHS: Record<string, string> = {
  ctrl: "Ctrl",
  alt: "Alt",
  shift: "Shift",
  super: "Super",
  cmd: "Super",
  space: "Space",
  escape: "Esc",
  left: "←",
  right: "→",
  up: "↑",
  down: "↓",
};

export const MOD_TOKENS = new Set([
  "ctrl",
  "alt",
  "shift",
  "super",
  "cmd",
  "fn",
]);

export const cap = (token: string): string =>
  GLYPHS[token] ?? (token.length === 1 ? token.toUpperCase() : token);

export const parseBinding = (raw: string): string[] =>
  raw
    .split("+")
    .map((t) => t.trim().toLowerCase())
    .filter(Boolean);

const MODIFIER_CODES: Record<string, string> = {
  ControlLeft: "ctrl",
  ControlRight: "ctrl",
  AltLeft: "alt",
  AltRight: "alt",
  ShiftLeft: "shift",
  ShiftRight: "shift",
  MetaLeft: "super",
  MetaRight: "super",
  OSLeft: "super",
  OSRight: "super",
};

const CODE_TOKENS: Record<string, string> = {
  Space: "space",
  Escape: "escape",
  Enter: "enter",
  NumpadEnter: "numpadenter",
  Tab: "tab",
  Backspace: "backspace",
  Delete: "delete",
  Insert: "insert",
  Home: "home",
  End: "end",
  PageUp: "pageup",
  PageDown: "pagedown",
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

const KEY_TOKENS: Record<string, string> = {
  " ": "space",
  "\u00A0": "space",
  Escape: "escape",
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  Control: "ctrl",
  Alt: "alt",
  Option: "alt",
  Shift: "shift",
  Meta: "super",
  OS: "super",
};

/**
 * Resolve a keyboard event to a binding token. Prefers `code` (physical key)
 * and falls back to `key` for synthetic events without a code.
 */
export const tokenFor = (event: { key: string; code?: string }): string => {
  const code = event.code ?? "";

  const modifier = MODIFIER_CODES[code];
  if (modifier) return modifier;

  const named = CODE_TOKENS[code];
  if (named) return named;

  if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^Numpad[0-9]$/.test(code)) return `num${code.slice(6)}`;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code.toLowerCase();

  return KEY_TOKENS[event.key] ?? event.key.toLowerCase();
};

/**
 * Turn the currently held tokens into the order the Rust parser expects:
 * canonical modifier order first, then exactly one non-modifier key.
 */
export const composeTokens = (held: Iterable<string>): string[] => {
  const tokens = new Set(held);
  const mods = ["ctrl", "alt", "shift", "super"].filter((m) => tokens.has(m));
  const keys = [...tokens].filter((t) => !MOD_TOKENS.has(t));
  return keys.length ? [...mods, keys[keys.length - 1]] : mods;
};

export const isModifierOnly = (raw: string): boolean => {
  const tokens = parseBinding(raw);
  return tokens.length > 0 && tokens.every((t) => MOD_TOKENS.has(t));
};
