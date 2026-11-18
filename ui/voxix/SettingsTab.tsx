import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useSettingsStore } from "@/stores/settingsStore";
import { useModelStore } from "@/stores/modelStore";
import { commands } from "@/bindings";
import { Group, Row, Screen, Segmented, Toggle, Slider, Btn } from "./ui";
import { HotKeyView } from "./HotKeyView";
import { isModifierOnly } from "./hotkeyTokens";
import { ModelSection } from "./ModelSection";
import {
  ClockIcon,
  ClipboardIcon,
  CopyIcon,
  GlobeIcon,
  HandTapIcon,
  MicIcon,
  MuteIcon,
  RefreshIcon,
  VolumeIcon,
} from "./icons";
import { LANGUAGES } from "@/lib/constants/languages";

export const SettingsTab = () => (
  <Screen title="Settings" caption="Recording, models, and app preferences.">
    <Group title="Transcription Model">
      <ModelSection />
    </Group>
    <LanguageSection />
    <HotkeySection />
    <MicSection />
    <SoundSection />
    <GeneralSection />
    <HistorySection />
  </Screen>
);

const LanguageSection = () => {
  const settings = useSettingsStore((s) => s.settings);
  const updateSetting = useSettingsStore((s) => s.updateSetting);
  const models = useModelStore((s) => s.models);
  const current = useModelStore((s) => s.currentModel);
  const model = models.find((m) => m.id === current);

  if (!model?.supports_language_selection) return null;

  return (
    <Group title="Output Language">
      <Row
        icon={<GlobeIcon size={16} />}
        title="Language"
        control={
          <select
            className="voxix-select"
            value={settings?.selected_language ?? "auto"}
            onChange={(e) => updateSetting("selected_language", e.target.value)}
          >
            {LANGUAGES.map((l) => (
              <option key={l.value} value={l.value}>
                {l.label}
              </option>
            ))}
          </select>
        }
      />
    </Group>
  );
};

const HotkeySection = () => {
  const settings = useSettingsStore((s) => s.settings);
  const updateSetting = useSettingsStore((s) => s.updateSetting);
  const binding = settings?.bindings?.transcribe?.current_binding ?? "";
  const modOnly = isModifierOnly(binding);
  const minTime = settings?.minimum_key_time ?? 0.2;

  return (
    <Group title="Hot Key">
      <div
        className="voxix-row"
        style={{ flexDirection: "column", alignItems: "stretch", gap: 4 }}
      >
        <HotKeyView id="transcribe" />
      </div>
      <Row
        icon={<MicIcon size={16} />}
        title="Push to talk"
        caption="How the hotkey starts and stops recording."
        control={
          <Segmented
            options={[
              { value: "hold", label: "Hold to talk" },
              { value: "tap", label: "Tap to toggle" },
            ]}
            value={settings?.push_to_talk === false ? "tap" : "hold"}
            onChange={(v) => updateSetting("push_to_talk", v === "hold")}
          />
        }
      />
      <Row
        icon={<HandTapIcon size={16} />}
        title="Enable double-tap lock"
        caption="Tap the hotkey twice to keep recording hands-free. Tap again to stop."
        control={
          <Toggle
            on={settings?.double_tap_lock_enabled ?? true}
            onChange={(v) => updateSetting("double_tap_lock_enabled", v)}
          />
        }
      />
      {!modOnly && (
        <Row
          icon={<HandTapIcon size={16} />}
          title="Use double-tap only"
          caption="Disable press-and-hold; only double-tap starts recording."
          control={
            <Toggle
              on={settings?.use_double_tap_only ?? false}
              onChange={(v) => updateSetting("use_double_tap_only", v)}
              disabled={!(settings?.double_tap_lock_enabled ?? true)}
            />
          }
        />
      )}
      {modOnly && (
        <Row
          icon={<ClockIcon size={16} />}
          title={`Ignore below ${minTime.toFixed(1)}s`}
          caption="Accidental taps shorter than this are discarded silently."
          control={
            <Slider
              value={minTime}
              min={0}
              max={2}
              step={0.1}
              onChange={(v) => updateSetting("minimum_key_time", v)}
            />
          }
        />
      )}
    </Group>
  );
};

const MicSection = () => {
  const { t } = useTranslation();
  const settings = useSettingsStore((s) => s.settings);
  const updateSetting = useSettingsStore((s) => s.updateSetting);
  const devices = useSettingsStore((s) => s.audioDevices);
  const refresh = useSettingsStore((s) => s.refreshAudioDevices);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const selected = settings?.selected_microphone ?? null;
  const missing = selected && !devices.some((d) => d.name === selected);

  return (
    <Group
      title="Microphone Selection"
      footer="Override the system default microphone with a specific input device. This setting will persist across sessions."
    >
      <Row
        icon={<MicIcon size={16} />}
        title="Input Device"
        caption={
          missing
            ? "Selected device not connected. System default will be used."
            : undefined
        }
        control={
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <select
              className="voxix-select"
              value={selected ?? ""}
              onChange={(e) =>
                updateSetting(
                  "selected_microphone",
                  e.target.value === "" ? null : e.target.value,
                )
              }
            >
              <option value="">{t("voxix.settings.systemDefault")}</option>
              {devices.map((d) => (
                <option key={d.index} value={d.name}>
                  {d.name}
                </option>
              ))}
              {missing && <option value={selected}>{selected}</option>}
            </select>
            <button
              type="button"
              className="voxix-icon-btn"
              title="Refresh available input devices"
              onClick={refresh}
            >
              <RefreshIcon size={14} />
            </button>
          </div>
        }
      />
    </Group>
  );
};

const SoundSection = () => {
  const settings = useSettingsStore((s) => s.settings);
  const updateSetting = useSettingsStore((s) => s.updateSetting);
  const on = settings?.audio_feedback ?? false;
  const volume = settings?.audio_feedback_volume ?? 0.65;

  return (
    <Group title="Sound">
      <Row
        icon={<VolumeIcon size={16} />}
        title="Sound Effects"
        control={
          <Toggle
            on={on}
            onChange={(v) => updateSetting("audio_feedback", v)}
          />
        }
      />
      <Row
        title="Volume"
        control={
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span
              className="voxix-cap tabular-nums"
              style={{ width: 32, textAlign: "right" }}
            >
              {Math.round(volume * 100)}%
            </span>
            <Slider
              value={volume}
              min={0}
              max={1}
              step={0.05}
              disabled={!on}
              onChange={(v) => {
                updateSetting("audio_feedback_volume", v);
                commands
                  .playTestSound("start")
                  .catch((e) => console.error("Failed to play test sound:", e));
              }}
            />
          </div>
        }
      />
    </Group>
  );
};

const GeneralSection = () => {
  const { t } = useTranslation();
  const settings = useSettingsStore((s) => s.settings);
  const updateSetting = useSettingsStore((s) => s.updateSetting);
  const pasteMethod = settings?.paste_method ?? "ctrl_v";
  const useClipboard = pasteMethod !== "direct";
  const mute = settings?.mute_while_recording ?? false;

  return (
    <Group title="General">
      <Row
        title="Open on Login"
        control={
          <Toggle
            on={settings?.autostart_enabled ?? false}
            onChange={(v) => updateSetting("autostart_enabled", v)}
          />
        }
      />
      <Row
        icon={<ClipboardIcon size={16} />}
        title="Use clipboard to insert"
        caption="Use clipboard to insert text. Fast but may not restore all clipboard content. Turn off to use simulated keypresses. Slower, but doesn't touch the clipboard."
        control={
          <Toggle
            on={useClipboard}
            onChange={(v) =>
              updateSetting("paste_method", v ? "ctrl_v" : "direct")
            }
          />
        }
      />
      <Row
        icon={<CopyIcon size={16} />}
        title="Copy to clipboard"
        caption="Copy transcription text to clipboard in addition to pasting it."
        control={
          <Toggle
            on={settings?.clipboard_handling === "copy_to_clipboard"}
            onChange={(v) =>
              updateSetting(
                "clipboard_handling",
                v ? "copy_to_clipboard" : "dont_modify",
              )
            }
          />
        }
      />
      <Row
        icon={<MuteIcon size={16} />}
        title="Audio Behavior while Recording"
        control={
          <select
            className="voxix-select"
            value={mute ? "mute" : "nothing"}
            onChange={(e) =>
              updateSetting("mute_while_recording", e.target.value === "mute")
            }
          >
            <option value="nothing">{t("voxix.settings.doNothing")}</option>
            <option value="mute">{t("voxix.settings.muteVolume")}</option>
          </select>
        }
      />
    </Group>
  );
};

const HistorySection = () => {
  const { t } = useTranslation();
  const settings = useSettingsStore((s) => s.settings);
  const updateSetting = useSettingsStore((s) => s.updateSetting);
  const limit = settings?.history_limit ?? 0;
  const enabled = limit !== 0;

  return (
    <Group
      title="History"
      footer={
        enabled
          ? undefined
          : "When disabled, transcriptions will not be saved and audio files will be deleted immediately after transcription."
      }
    >
      <Row
        icon={<ClockIcon size={16} />}
        title="Save Transcription History"
        caption="Save transcriptions and audio recordings for later access."
        control={
          <Toggle
            on={enabled}
            onChange={(v) => {
              updateSetting("save_history", v);
              updateSetting("history_limit", v ? 100 : 0);
            }}
          />
        }
      />
      {enabled && (
        <Row
          title="Maximum History Entries"
          caption="Oldest entries will be automatically deleted when limit is reached."
          control={
            <select
              className="voxix-select"
              value={limit}
              onChange={(e) => {
                const v = Number(e.target.value);
                updateSetting("history_limit", v);
              }}
            >
              <option value={-1}>{t("voxix.settings.unlimited")}</option>
              <option value={50}>50</option>
              <option value={100}>100</option>
              <option value={200}>200</option>
              <option value={500}>500</option>
              <option value={1000}>1000</option>
            </select>
          }
        />
      )}
    </Group>
  );
};
