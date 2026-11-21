import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import {
  cap,
  composeTokens,
  MOD_TOKENS,
  parseBinding,
  tokenFor,
} from "./hotkeyTokens";

/**
 * voxix-keys names modifiers per-platform: macOS emits "option"/"command",
 * Linux/Windows emit "alt"/"super". Normalize to our canonical token names so
 * stored bindings (e.g. "alt+space") stay consistent across platforms and
 * match `hotkeyTokens.composeTokens`.
 */
const MOD_ALIASES: Record<string, string> = {
  option: "alt",
  command: "super",
  control: "ctrl",
};

type VoxixKeysEvent = {
  modifiers: string[];
  key: string | null;
  is_key_down: boolean;
  hotkey_string: string;
};

export const HotKeyView = ({ id, hint }: { id: string; hint?: string }) => {
  const { t } = useTranslation();
  const binding = useSettingsStore(
    (s) => s.settings?.bindings?.[id]?.current_binding ?? "",
  );
  const updateBinding = useSettingsStore((s) => s.updateBinding);
  const keyboardImpl: "tauri" | "voxix_keys" = useSettingsStore(
    (s) => s.settings?.keyboard_implementation ?? "tauri",
  );
  const [capturing, setCapturing] = useState(false);
  const [pending, setPending] = useState<string[]>([]);
  const [hintText, setHintText] = useState<string | null>(null);
  const unlisten = useRef<(() => void) | null>(null);
  const finishing = useRef(false);
  const pendingRef = useRef<string[]>([]);

  useEffect(() => {
    return () => {
      if (unlisten.current) {
        unlisten.current();
        unlisten.current = null;
      }
    };
  }, []);

  const stopCapture = useCallback(async () => {
    setCapturing(false);
    if (unlisten.current) {
      unlisten.current();
      unlisten.current = null;
    }
    // The Tauri backend captures in the webview; there is no Rust-side
    // recording session to stop.
    if (keyboardImpl !== "voxix_keys") return;
    const result = await commands
      .stopVoxixKeysRecording()
      .catch((e) => ({ status: "error" as const, error: String(e) }));
    if (result.status === "error") {
      console.error("Failed to stop voxix-keys recording:", result.error);
    }
  }, [keyboardImpl]);

  const commit = useCallback(
    async (tokens: string[]) => {
      if (finishing.current) return;
      finishing.current = true;
      await stopCapture();
      if (!tokens.length) return;

      if (
        tokens.every((t) => MOD_TOKENS.has(t)) &&
        keyboardImpl !== "voxix_keys"
      ) {
        // The Tauri backend requires a main key; voxix-keys binds bare
        // modifier chords, so only refuse when the engine cannot do them.
        setPending(tokens);
        setHintText("Add a non-modifier key (e.g. Alt+Space)");
        finishing.current = false;
        setCapturing(true);
        return;
      }
      pendingRef.current = [];

      try {
        await updateBinding(id, tokens.join("+"));
      } catch (e) {
        toast.error("Could not set shortcut", {
          id: `hotkey-${id}`,
          description: e instanceof Error ? e.message : String(e),
        });
      }
    },
    [id, keyboardImpl, updateBinding, stopCapture],
  );

  useEffect(() => {
    if (!capturing) return;
    finishing.current = false;
    setPending([]);
    setHintText(null);

    // Tauri backend: the user clicked this field, so the webview has focus.
    // Capture with a DOM keydown listener — no Accessibility permission and
    // no global keyboard hook required.
    if (keyboardImpl === "tauri") {
      const held = new Set<string>();
      const onKeyDown = (event: KeyboardEvent) => {
        event.preventDefault();
        event.stopPropagation();
        if (event.repeat) return;

        const token = tokenFor(event);
        if (token === "escape") {
          void stopCapture();
          return;
        }
        if (MOD_TOKENS.has(token)) {
          held.add(token);
          setPending(composeTokens(held));
          setHintText("Add a non-modifier key (e.g. Alt+Space)");
          return;
        }
        void commit(composeTokens(new Set([...held, token])));
      };
      const onKeyUp = (event: KeyboardEvent) => {
        const token = tokenFor(event);
        if (MOD_TOKENS.has(token)) held.delete(token);
      };
      window.addEventListener("keydown", onKeyDown, true);
      window.addEventListener("keyup", onKeyUp, true);
      return () => {
        window.removeEventListener("keydown", onKeyDown, true);
        window.removeEventListener("keyup", onKeyUp, true);
      };
    }

    let cancelled = false;
    (async () => {
      unlisten.current = await listen<VoxixKeysEvent>(
        "voxix-keys-event",
        (event) => {
          if (cancelled) return;
          const ev = event.payload;

          if (!ev.is_key_down) {
            // voxix-keys: a bare-modifier chord commits when the last modifier
            // is released with a modifier-only pending set (Alt+Super, etc).
            if (keyboardImpl === "voxix_keys") {
              const mods = ev.modifiers
                .map((m) => MOD_ALIASES[m] ?? m)
                .filter((t) => MOD_TOKENS.has(t));
              if (mods.length === 0 && pendingRef.current.length >= 2) {
                commit(pendingRef.current);
              }
            }
            return;
          }

          // Escape always aborts capture.
          if (ev.key === "escape") {
            stopCapture();
            return;
          }

          // Normalize per-platform modifier names to our canonical tokens.
          const mods = ev.modifiers
            .map((m) => MOD_ALIASES[m] ?? m)
            .filter((t) => MOD_TOKENS.has(t));

          if (!ev.key) {
            // Modifier held with no main key yet: show pending, keep capture open.
            pendingRef.current = mods;
            setPending(mods);
            setHintText(
              mods.length
                ? keyboardImpl === "voxix_keys"
                  ? "Release all keys to set a bare modifier chord"
                  : "Add a non-modifier key (e.g. Alt+Space)"
                : null,
            );
            return;
          }

          // Full chord: modifiers + main key.
          commit([...mods, ev.key]);
        },
      );

      if (cancelled) {
        unlisten.current?.();
        unlisten.current = null;
        return;
      }

      await commands.startVoxixKeysRecording(id).catch((e) => {
        toast.error("Could not start shortcut capture", {
          description: e instanceof Error ? e.message : String(e),
        });
        stopCapture();
      });
    })();

    return () => {
      cancelled = true;
    };
  }, [capturing, id, commit, stopCapture, keyboardImpl]);

  const start = async () => {
    setPending([]);
    setHintText(null);
    finishing.current = false;
    setCapturing(true);
  };

  const tokens = capturing ? pending : parseBinding(binding);
  const hyper =
    tokens.filter((t) => MOD_TOKENS.has(t)).length === 4 && tokens.length === 4;
  const placeholder = hintText ?? hint ?? "Enter a key combination";

  return (
    <button
      type="button"
      onClick={start}
      className="voxix-hotkey-btn"
      data-capturing={capturing ? "true" : undefined}
    >
      {tokens.length === 0 && (
        <span className="voxix-cap">{capturing ? placeholder : "Not set"}</span>
      )}
      {hyper ? (
        <span className="voxix-keycap">{t("voxix.hotKeyView.hyper")}</span>
      ) : (
        tokens.map((t, i) => (
          <span key={`${t}-${i}`} className="voxix-keycap">
            {cap(t)}
          </span>
        ))
      )}
    </button>
  );
};
