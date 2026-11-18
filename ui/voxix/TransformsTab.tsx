import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSettingsStore } from "@/stores/settingsStore";
import type { AppSettings } from "@/bindings";
import { Group, Row, Screen, Segmented, Toggle, Btn } from "./ui";
import { TrashIcon } from "./icons";

const applyTransforms = (
  text: string,
  s: Pick<
    AppSettings,
    | "word_removals_enabled"
    | "word_removal_patterns"
    | "word_remappings"
    | "lowercase_output"
    | "remove_punctuation"
  >,
): string => {
  let out = text;
  if (s.word_removals_enabled) {
    for (const pat of s.word_removal_patterns ?? []) {
      if (!pat) continue;
      try {
        out = out.replace(new RegExp(`\\b${pat}\\b`, "gi"), "");
      } catch {
        // invalid regex: skip
      }
    }
  }
  for (const r of s.word_remappings ?? []) {
    if (!r.enabled || !r.pattern) continue;
    try {
      const flags =
        r.pattern.startsWith("(?i)") || r.pattern.startsWith("(?-i)")
          ? "g"
          : "gi";
      const pat = r.pattern.startsWith("(?i)") ? r.pattern.slice(4) : r.pattern;
      out = out.replace(new RegExp(pat, flags), r.replacement);
    } catch {
      // invalid regex: skip
    }
  }
  if (s.remove_punctuation) out = out.replace(/[.,!?;:"'()\[\]{}—–-]/g, "");
  out = out.replace(/\s+/g, " ").trim();
  if (s.lowercase_output) out = out.toLowerCase();
  return out;
};

const isValidRegex = (pat: string): boolean => {
  if (!pat) return true;
  try {
    new RegExp(pat);
    return true;
  } catch {
    return false;
  }
};

const SAMPLE_PRESETS = [
  "Hello, World! This is a TEST.",
  "hello comma world",
  "Um, like, you know, I was thinking...",
];

export const TransformsTab = () => {
  const { t } = useTranslation();
  const settings = useSettingsStore((s) => s.settings);
  const updateSetting = useSettingsStore((s) => s.updateSetting);
  const [sample, setSample] = useState(SAMPLE_PRESETS[0]);
  const [mode, setMode] = useState<"remove" | "replace">("remove");

  const output = useMemo(
    () => (settings ? applyTransforms(sample, settings) : ""),
    [sample, settings],
  );

  if (!settings) return null;

  const patterns = settings.word_removal_patterns ?? [];
  const remappings = settings.word_remappings ?? [];

  return (
    <Screen
      title="Transforms"
      caption="Rules applied to every transcript before it is pasted."
    >
      <div>
        <div className="voxix-section-title">
          {t("voxix.transforms.preview")}
        </div>
        <div
          className="voxix-card"
          style={{
            padding: 16,
            display: "flex",
            flexDirection: "column",
            gap: 10,
          }}
        >
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <span className="voxix-cap" style={{ fontWeight: 500 }}>
              {t("voxix.transforms.inputLabel")}
            </span>
            <input
              className="voxix-input"
              placeholder="Type a sample sentence…"
              value={sample}
              onChange={(e) => setSample(e.target.value)}
              style={{ flex: 1, minWidth: 0 }}
            />
          </div>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            {SAMPLE_PRESETS.map((p) => (
              <button
                key={p}
                type="button"
                className="voxix-btn"
                style={{ fontSize: "var(--text-xs)", padding: "3px 8px" }}
                onClick={() => setSample(p)}
              >
                {p.slice(0, 18)}…
              </button>
            ))}
          </div>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              justifyContent: "center",
              color: "var(--text-muted)",
              fontSize: "var(--text-xs)",
            }}
          >
            <span style={{ flex: 1, height: 1, background: "var(--border)" }} />
            <span>{t("voxix.transforms.transformsTo")}</span>
            <span style={{ flex: 1, height: 1, background: "var(--border)" }} />
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <span className="voxix-cap" style={{ fontWeight: 500 }}>
              {t("voxix.transforms.outputLabel")}
            </span>
            <div
              style={{
                borderRadius: 8,
                padding: "10px 12px",
                minHeight: 36,
                fontSize: "var(--text-base)",
                background: "var(--bg)",
                border: "1px solid var(--border)",
                lineHeight: "var(--leading-base)",
                wordBreak: "break-word",
                whiteSpace: "pre-wrap",
                userSelect: "text",
                cursor: "text",
              }}
            >
              {output || (
                <span style={{ color: "var(--text-muted)" }}>
                  {t("voxix.transforms.noOutput")}
                </span>
              )}
            </div>
          </div>
          {sample && output !== sample && (
            <div className="voxix-cap" style={{ textAlign: "center" }}>
              {t("voxix.transforms.charCount", {
                input: sample.length,
                output: output.length,
              })}
            </div>
          )}
          <div
            className="voxix-cap"
            style={{ textAlign: "center", fontStyle: "italic" }}
          >
            {t("voxix.transforms.futureTranscriptsOnly")}
          </div>
        </div>
      </div>

      <div>
        <div className="voxix-section-title">
          {t("voxix.transforms.wordRules")}
        </div>
        <div style={{ marginBottom: 12 }}>
          <Segmented
            options={[
              { value: "remove", label: "Remove words" },
              { value: "replace", label: "Replace words" },
            ]}
            value={mode}
            onChange={setMode}
          />
        </div>

        {mode === "remove" && (
          <Group
            caption="Remove matching words from every transcript."
            footer="Patterns are regular expressions matched against whole words (case-insensitive)."
          >
            <Row
              title="Enabled"
              caption={
                settings.word_removals_enabled
                  ? "Removals active"
                  : "Removals paused"
              }
              control={
                <Toggle
                  on={settings.word_removals_enabled ?? false}
                  onChange={(v) => updateSetting("word_removals_enabled", v)}
                />
              }
            />
            {patterns.length === 0 ? (
              <div className="voxix-row" style={{ justifyContent: "center" }}>
                <span className="voxix-cap">
                  {t("voxix.transforms.noPatterns")}
                </span>
              </div>
            ) : (
              patterns.map((p, i) => {
                const valid = isValidRegex(p);
                return (
                  <Row
                    key={i}
                    title={
                      <input
                        className="voxix-input"
                        style={{
                          fontFamily: "var(--font-mono)",
                          borderColor: valid ? undefined : "var(--danger)",
                          flex: 1,
                        }}
                        placeholder="Regex, e.g. um|uh"
                        value={p}
                        onChange={(e) => {
                          const next = [...patterns];
                          next[i] = e.target.value;
                          updateSetting("word_removal_patterns", next);
                        }}
                      />
                    }
                    caption={!valid ? "Invalid regex" : undefined}
                    control={
                      <button
                        type="button"
                        className="voxix-icon-btn danger"
                        title="Delete"
                        onClick={() =>
                          updateSetting(
                            "word_removal_patterns",
                            patterns.filter((_, j) => j !== i),
                          )
                        }
                      >
                        <TrashIcon size={14} />
                      </button>
                    }
                  />
                );
              })
            )}
            <div className="voxix-row" style={{ justifyContent: "flex-start" }}>
              <Btn
                onClick={() =>
                  updateSetting("word_removal_patterns", [...patterns, ""])
                }
              >
                {t("voxix.transforms.addPattern")}
              </Btn>
            </div>
          </Group>
        )}

        {mode === "replace" && (
          <Group caption="Replace matching text. Replacements are applied in order.">
            {remappings.length === 0 ? (
              <div className="voxix-row" style={{ justifyContent: "center" }}>
                <span className="voxix-cap">
                  {t("voxix.transforms.noReplacements")}
                </span>
              </div>
            ) : (
              remappings.map((r, i) => {
                const valid = isValidRegex(r.pattern);
                return (
                  <div
                    key={i}
                    className="voxix-row"
                    style={{ gap: 8, alignItems: "center" }}
                  >
                    <Toggle
                      on={r.enabled}
                      onChange={(v) => {
                        const next = [...remappings];
                        next[i] = { ...r, enabled: v };
                        updateSetting("word_remappings", next);
                      }}
                    />
                    <input
                      className="voxix-input"
                      style={{
                        fontFamily: "var(--font-mono)",
                        flex: "1 1 0",
                        minWidth: 0,
                        borderColor: valid ? undefined : "var(--danger)",
                      }}
                      placeholder="Match"
                      value={r.pattern}
                      onChange={(e) => {
                        const next = [...remappings];
                        next[i] = { ...r, pattern: e.target.value };
                        updateSetting("word_remappings", next);
                      }}
                    />
                    <span
                      style={{
                        color: "var(--text-muted)",
                        flexShrink: 0,
                        fontSize: "var(--text-base)",
                      }}
                    >
                      →
                    </span>
                    <input
                      className="voxix-input"
                      style={{
                        fontFamily: "var(--font-mono)",
                        flex: "1 1 0",
                        minWidth: 0,
                      }}
                      placeholder="Replace"
                      value={r.replacement}
                      onChange={(e) => {
                        const next = [...remappings];
                        next[i] = { ...r, replacement: e.target.value };
                        updateSetting("word_remappings", next);
                      }}
                    />
                    <button
                      type="button"
                      className="voxix-icon-btn danger"
                      title="Delete"
                      style={{ flexShrink: 0 }}
                      onClick={() =>
                        updateSetting(
                          "word_remappings",
                          remappings.filter((_, j) => j !== i),
                        )
                      }
                    >
                      <TrashIcon size={14} />
                    </button>
                  </div>
                );
              })
            )}
            <div className="voxix-row">
              <Btn
                onClick={() =>
                  updateSetting("word_remappings", [
                    ...remappings,
                    { enabled: true, pattern: "", replacement: "" },
                  ])
                }
              >
                {t("voxix.transforms.addReplacement")}
              </Btn>
            </div>
          </Group>
        )}
      </div>

      <div>
        <div className="voxix-section-title">
          {t("voxix.transforms.outputFormatting")}
        </div>
        <div className="voxix-card">
          <Row
            title="Lowercase output"
            caption="Convert entire transcript to lowercase"
            control={
              <Toggle
                on={settings.lowercase_output ?? false}
                onChange={(v) => updateSetting("lowercase_output", v)}
              />
            }
          />
          <Row
            title="Remove punctuation"
            caption="Strip . , ! ? ; : ( ) [ ] { } — – -"
            control={
              <Toggle
                on={settings.remove_punctuation ?? false}
                onChange={(v) => updateSetting("remove_punctuation", v)}
              />
            }
          />
        </div>
      </div>
    </Screen>
  );
};
