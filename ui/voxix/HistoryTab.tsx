import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { convertFileSrc } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { toast } from "sonner";
import { commands, events, type HistoryEntry } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { Btn, Screen, Segmented, useEscape } from "./ui";
import {
  ClockIcon,
  CopyIcon,
  CheckIcon,
  PlayIcon,
  PauseIcon,
  TrashIcon,
  RefreshIcon,
  XIcon,
  ExclamationTriangle,
} from "./icons";

const relDate = (ts: number): string => {
  const d = new Date(ts * 1000);
  const now = new Date();
  const day = (x: Date) =>
    new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(now) - day(d)) / 86400000);
  if (diff <= 0) return "Today";
  if (diff === 1) return "Yesterday";
  if (diff < 7) return d.toLocaleDateString(undefined, { weekday: "long" });
  return d.toLocaleDateString(undefined, { dateStyle: "medium" });
};

const time = (ts: number) =>
  new Date(ts * 1000).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });

const fmtClock = (s: number) => {
  if (!Number.isFinite(s) || s < 0) return "0:00";
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${sec.toString().padStart(2, "0")}`;
};

const MiniPlayer = ({
  entry,
  playing,
  progress,
  onToggle,
  onSeek,
  onStop,
}: {
  entry: HistoryEntry;
  playing: boolean;
  progress: { cur: number; dur: number };
  onToggle: () => void;
  onSeek: (v: number) => void;
  onStop: () => void;
}) => (
  <div
    style={{
      position: "sticky",
      bottom: 0,
      zIndex: 5,
      display: "flex",
      alignItems: "center",
      gap: 12,
      padding: "10px 14px",
      background: "var(--surface)",
      border: "1px solid var(--border)",
      borderRadius: 10,
      boxShadow: "0 -8px 24px rgba(0, 0, 0, 0.35)",
      flexShrink: 0,
    }}
    role="region"
    aria-label="Audio player"
  >
    <button
      type="button"
      className="voxix-icon-btn"
      onClick={onToggle}
      aria-label={playing ? "Pause" : "Play"}
      style={{ color: "var(--accent)" }}
    >
      {playing ? <PauseIcon size={16} /> : <PlayIcon size={16} />}
    </button>
    <div style={{ minWidth: 0, width: 140, flexShrink: 0 }}>
      <div
        style={{
          fontSize: "var(--text-xs)",
          whiteSpace: "nowrap",
          overflow: "hidden",
          textOverflow: "ellipsis",
        }}
      >
        {entry.transcription_text?.trim() || "Recording"}
      </div>
      <div className="voxix-cap">
        {relDate(entry.timestamp)} · {time(entry.timestamp)}
      </div>
    </div>
    <span className="voxix-cap" style={{ width: 34, flexShrink: 0 }}>
      {fmtClock(progress.cur)}
    </span>
    <input
      type="range"
      min={0}
      max={Math.max(progress.dur, 0.1)}
      step={0.05}
      value={Math.min(progress.cur, progress.dur)}
      onChange={(e) => onSeek(Number(e.target.value))}
      style={{ flex: 1, accentColor: "var(--accent)", minWidth: 0 }}
      aria-label="Seek"
    />
    <span
      className="voxix-cap"
      style={{ width: 34, flexShrink: 0, textAlign: "right" }}
    >
      {fmtClock(progress.dur)}
    </span>
    <button
      type="button"
      className="voxix-icon-btn"
      onClick={onStop}
      aria-label="Stop playback"
      title="Stop"
    >
      <XIcon size={14} />
    </button>
  </div>
);

const isImageUrl = (s: string): boolean =>
  s.startsWith("http") || s.startsWith("data:image");

type ClientIconEditorState = { label: string; initial: string | null };

const ClientIconEditor = ({
  label,
  initial,
  onClose,
  onSaved,
}: ClientIconEditorState & {
  onClose: () => void;
  onSaved: () => void;
}) => {
  const initialIsImage = initial ? isImageUrl(initial) : false;
  const [mode, setMode] = useState<"text" | "image">(
    initialIsImage ? "image" : "text",
  );
  const [value, setValue] = useState(initial ?? "");
  const [saving, setSaving] = useState(false);

  useEscape(true, onClose);

  const trimmed = value.trim();
  const canSave = mode === "text" ? trimmed.length > 0 : isImageUrl(trimmed);

  const changeMode = (next: "text" | "image") => {
    if (next === mode) return;
    setMode(next);
    setValue((current) => {
      const image = isImageUrl(current.trim());
      if (next === "image" && !image) return "";
      if (next === "text" && image) return "";
      return current;
    });
  };

  const save = async (icon: string | null) => {
    if (saving) return;
    setSaving(true);
    try {
      const res = await commands.setClientIcon(label, icon);
      if (res.status === "ok") {
        toast.success(icon ? "Icon assigned" : "Icon reset", {
          id: `client-icon-${label}`,
          description: label,
        });
        onSaved();
        onClose();
      } else {
        toast.error("Failed to save icon", {
          id: `client-icon-${label}`,
          description: String(res.error),
        });
      }
    } catch (e) {
      toast.error("Failed to save icon", {
        id: `client-icon-${label}`,
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="voxix-sheet-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={`Icon for ${label}`}
      onClick={onClose}
    >
      <div
        className="voxix-card"
        style={{
          padding: 16,
          width: 380,
          display: "flex",
          flexDirection: "column",
          gap: 10,
          boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
        }}
        onClick={(ev) => ev.stopPropagation()}
      >
        <div style={{ fontSize: "var(--text-base)", fontWeight: 600 }}>
          Client icon
        </div>
        <div className="voxix-cap">
          Shown next to every entry from <strong>{label}</strong>.
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <span
            style={{
              width: 36,
              height: 36,
              flexShrink: 0,
              display: "inline-flex",
              alignItems: "center",
              justifyContent: "center",
              overflow: "hidden",
              borderRadius: 6,
              border: "1px dashed var(--border)",
              background: "var(--surface-raised)",
              fontSize: 18,
            }}
            aria-hidden="true"
          >
            {trimmed ? (
              isImageUrl(trimmed) ? (
                <img
                  src={trimmed}
                  alt=""
                  style={{
                    width: "100%",
                    height: "100%",
                    objectFit: "cover",
                  }}
                />
              ) : (
                <span>{trimmed}</span>
              )
            ) : (
              <span style={{ color: "var(--text-muted)", fontSize: 12 }}>
                none
              </span>
            )}
          </span>
          <Segmented
            options={[
              { value: "text", label: "Emoji / text" },
              { value: "image", label: "Image" },
            ]}
            value={mode}
            onChange={changeMode}
          />
        </div>
        <input
          className="voxix-input"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder={
            mode === "text" ? "🖥 or PC" : "https://… or data:image/…"
          }
          maxLength={mode === "text" ? 64 : 262144}
          autoFocus
          aria-label={
            mode === "text" ? "Emoji or short text" : "Image URL or data URI"
          }
        />
        <div className="voxix-cap">
          {mode === "text"
            ? "Use an emoji or a few characters."
            : "Paste an image URL or a data:image URI."}
        </div>
        <div
          style={{
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
            marginTop: 2,
          }}
        >
          <Btn onClick={onClose}>Cancel</Btn>
          <Btn onClick={() => void save(null)} disabled={saving || !initial}>
            Reset
          </Btn>
          <Btn
            variant="primary"
            onClick={() => void save(trimmed)}
            disabled={saving || !canSave}
          >
            Save
          </Btn>
        </div>
      </div>
    </div>
  );
};

export const HistoryTab = () => {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [confirming, setConfirming] = useState(false);
  const [iconEditor, setIconEditor] = useState<ClientIconEditorState | null>(
    null,
  );
  const saveHistory = useSettingsStore((s) => s.settings?.save_history ?? true);

  const audioRef = useRef<HTMLAudioElement | null>(null);
  const playingRef = useRef(false);
  const [playerEntry, setPlayerEntry] = useState<HistoryEntry | null>(null);
  const [playerPlaying, setPlayerPlaying] = useState(false);
  const [progress, setProgress] = useState({ cur: 0, dur: 0 });

  const load = useCallback(async () => {
    try {
      const res = await commands.getHistoryEntries(null, 100);
      if (res.status === "ok")
        setEntries(
          [...res.data.entries].sort((a, b) => b.timestamp - a.timestamp),
        );
      else
        toast.error("Failed to load history", {
          description: String(res.error),
        });
    } catch (e) {
      toast.error("Failed to load history", {
        description: e instanceof Error ? e.message : String(e),
      });
    }
  }, []);

  useEffect(() => {
    load();
    const un = events.historyUpdatePayload.listen(() => load());
    // The transcription service writes its own history row straight into
    // history.db (mirroring save_entry) when the batch path runs in the
    // daemon process — it cannot emit the Tauri `history-update-payload`
    // event, so the list would stay stale until the tab remounts. Poll the
    // local SQLite store while the tab is mounted so a finished recording
    // shows up immediately.
    const poll = window.setInterval(() => {
      void load();
    }, 1000);
    return () => {
      window.clearInterval(poll);
      un.then((f) => f());
    };
  }, [load]);

  const stopPlayer = useCallback(() => {
    audioRef.current?.pause();
    audioRef.current = null;
    playingRef.current = false;
    setPlayerPlaying(false);
    setPlayerEntry(null);
    setProgress({ cur: 0, dur: 0 });
  }, []);

  const stopPlayerIfCurrent = useCallback(
    (entry: HistoryEntry) => {
      if (playerEntry?.id === entry.id) stopPlayer();
    },
    [playerEntry, stopPlayer],
  );

  const togglePlayer = useCallback(
    async (entry: HistoryEntry) => {
      const active = audioRef.current;
      if (playerEntry?.id === entry.id && active) {
        if (playingRef.current) {
          active.pause();
          playingRef.current = false;
          setPlayerPlaying(false);
        } else {
          try {
            await active.play();
            playingRef.current = true;
            setPlayerPlaying(true);
          } catch {
            playingRef.current = false;
            setPlayerPlaying(false);
            toast.error("Failed to play audio", { id: `audio-${entry.id}` });
          }
        }
        return;
      }

      active?.pause();
      audioRef.current = null;
      if (!entry.file_name) {
        toast.error("No audio file for this entry", {
          id: `audio-${entry.id}`,
        });
        return;
      }
      const res = await commands.getAudioFilePath(entry.file_name);
      if (res.status !== "ok" || !res.data) {
        toast.error("Audio file not found", {
          id: `audio-${entry.id}`,
          description:
            res.status === "error"
              ? String(res.error)
              : "File may have been deleted",
        });
        return;
      }
      const el = new Audio(convertFileSrc(res.data));
      el.onloadedmetadata = () =>
        setProgress({
          cur: 0,
          dur: Number.isFinite(el.duration) ? el.duration : 0,
        });
      el.ontimeupdate = () =>
        setProgress((p) => ({ ...p, cur: el.currentTime }));
      el.onended = () => stopPlayer();
      el.onerror = () => {
        playingRef.current = false;
        setPlayerPlaying(false);
        toast.error("Failed to play audio", { id: `audio-${entry.id}` });
      };
      audioRef.current = el;
      setPlayerEntry(entry);
      setProgress({ cur: 0, dur: 0 });
      playingRef.current = true;
      setPlayerPlaying(true);
      try {
        await el.play();
      } catch {
        playingRef.current = false;
        setPlayerPlaying(false);
        toast.error("Failed to play audio", { id: `audio-${entry.id}` });
      }
    },
    [playerEntry, stopPlayer],
  );

  useEffect(
    () => () => {
      audioRef.current?.pause();
      audioRef.current = null;
    },
    [],
  );

  useEscape(confirming, () => setConfirming(false));

  if (!saveHistory) {
    return (
      <Empty
        title="History Disabled"
        body="Transcription history is currently disabled."
      />
    );
  }

  if (entries.length === 0) {
    return (
      <Empty
        title="No Transcriptions"
        body="Your transcription history will appear here."
      />
    );
  }

  return (
    <Screen
      title="History"
      caption="Your saved transcriptions and audio recordings."
    >
      <div style={{ display: "flex", justifyContent: "flex-end" }}>
        <Btn variant="destructive" onClick={() => setConfirming(true)}>
          {t("voxix.history.deleteAll")}
        </Btn>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
        {entries.map((e) => (
          <Card
            key={e.id}
            entry={e}
            playing={playerEntry?.id === e.id && playerPlaying}
            onTogglePlay={togglePlayer}
            onDeleted={stopPlayerIfCurrent}
            onEditClientIcon={(entry) => {
              if (!entry.client_label) return;
              setIconEditor({
                label: entry.client_label,
                initial: entry.client_icon,
              });
            }}
          />
        ))}
      </div>
      {playerEntry && (
        <MiniPlayer
          entry={playerEntry}
          playing={playerPlaying}
          progress={progress}
          onToggle={() => void togglePlayer(playerEntry)}
          onSeek={(v) => {
            if (audioRef.current) {
              audioRef.current.currentTime = v;
              setProgress((p) => ({ ...p, cur: v }));
            }
          }}
          onStop={stopPlayer}
        />
      )}
      {confirming && (
        <div
          className="voxix-sheet-backdrop"
          role="dialog"
          aria-modal="true"
          onClick={() => setConfirming(false)}
        >
          <div
            className="voxix-card"
            style={{
              padding: 16,
              width: 320,
              boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
            }}
            onClick={(ev) => ev.stopPropagation()}
          >
            <div style={{ fontSize: "var(--text-base)", fontWeight: 600 }}>
              {t("voxix.history.deleteAllTitle")}
            </div>
            <div className="voxix-cap" style={{ marginTop: 4 }}>
              {t("voxix.history.deleteAllBody")}
            </div>
            <div
              style={{
                display: "flex",
                justifyContent: "flex-end",
                gap: 8,
                marginTop: 12,
              }}
            >
              <Btn onClick={() => setConfirming(false)}>
                {t("voxix.history.cancel")}
              </Btn>
              <Btn
                variant="destructive-primary"
                onClick={async () => {
                  setConfirming(false);
                  try {
                    const res = await commands.deleteAllHistory();
                    if (res.status === "error") {
                      toast.error("Failed to delete history", {
                        description: String(res.error),
                      });
                      return;
                    }
                  } catch (e) {
                    toast.error("Failed to delete history", {
                      description: e instanceof Error ? e.message : String(e),
                    });
                    return;
                  }
                  load();
                }}
              >
                {t("voxix.history.deleteAll")}
              </Btn>
            </div>
          </div>
        </div>
      )}
      {iconEditor && (
        <ClientIconEditor
          label={iconEditor.label}
          initial={iconEditor.initial}
          onClose={() => setIconEditor(null)}
          onSaved={() => void load()}
        />
      )}
    </Screen>
  );
};

const Empty = ({ title, body }: { title: string; body: string }) => (
  <div
    style={{
      display: "flex",
      flexDirection: "column",
      alignItems: "center",
      gap: 6,
      padding: "80px 16px",
      textAlign: "center",
    }}
  >
    <span style={{ color: "var(--text-muted)" }}>
      <ClockIcon size={28} />
    </span>
    <div style={{ fontSize: "var(--text-lg)", fontWeight: 600 }}>{title}</div>
    <div className="voxix-cap">{body}</div>
  </div>
);

const Card = ({
  entry,
  playing,
  onTogglePlay,
  onDeleted,
  onEditClientIcon,
}: {
  entry: HistoryEntry;
  playing: boolean;
  onTogglePlay: (entry: HistoryEntry) => void;
  onDeleted: (entry: HistoryEntry) => void;
  onEditClientIcon: (entry: HistoryEntry) => void;
}) => {
  const { t } = useTranslation();
  const [copyState, setCopyState] = useState<"idle" | "success" | "error">(
    "idle",
  );
  const [retrying, setRetrying] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const blank = !entry.transcription_text?.trim();

  const copy = async () => {
    try {
      await writeText(entry.transcription_text);
      setCopyState("success");
      toast.success("Copied to clipboard", { id: `copy-${entry.id}` });
    } catch (e) {
      setCopyState("error");
      const description = e instanceof Error ? e.message : String(e);
      toast.error("Copy failed", { id: `copy-${entry.id}`, description });
    } finally {
      window.setTimeout(() => setCopyState("idle"), 1500);
    }
  };

  const del = () => setConfirmDelete(true);

  const retry = async () => {
    if (retrying) return;
    setRetrying(true);
    try {
      const res = await commands.retryHistoryEntryTranscription(entry.id);
      const { toast } = await import("sonner");
      if (res.status === "ok") {
        toast.success("Transcription retried", { id: `retry-${entry.id}` });
      } else {
        toast.error("Retry failed", {
          id: `retry-${entry.id}`,
          description: String(res.error),
        });
      }
    } catch (e) {
      toast.error("Retry failed", {
        id: `retry-${entry.id}`,
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setRetrying(false);
    }
  };

  return (
    <div
      style={{
        background: "var(--surface)",
        border: "1px solid var(--border)",
        borderRadius: 10,
        padding: 12,
        minWidth: 0,
        overflow: "hidden",
      }}
    >
      <div
        style={{
          fontSize: "var(--text-base)",
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          userSelect: "text",
        }}
      >
        {blank ? (
          <span style={{ color: "var(--text-muted)", fontStyle: "italic" }}>
            {t("voxix.history.transcriptionFailedAudioKept")}
          </span>
        ) : (
          entry.transcription_text
        )}
      </div>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 12,
          marginTop: 8,
          paddingTop: 8,
          borderTop: "1px solid var(--border)",
        }}
      >
        <span className="voxix-cap">{relDate(entry.timestamp)}</span>
        <span className="voxix-cap">{time(entry.timestamp)}</span>
        {entry.client_label && (
          <button
            type="button"
            className="voxix-cap"
            onClick={() => onEditClientIcon(entry)}
            title={`Set icon for ${entry.client_label}`}
            aria-label={`Set icon for ${entry.client_label}`}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 4,
              padding: "1px 6px",
              margin: 0,
              borderRadius: 4,
              background: "var(--surface-raised)",
              border: "1px solid var(--border)",
              color: "inherit",
              cursor: "pointer",
              fontFamily: "inherit",
              fontSize: "var(--text-xs)",
              maxWidth: 120,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {entry.client_icon ? (
              isImageUrl(entry.client_icon) ? (
                <img
                  src={entry.client_icon}
                  alt=""
                  style={{
                    width: 12,
                    height: 12,
                    borderRadius: 2,
                    objectFit: "cover",
                  }}
                />
              ) : (
                <span aria-hidden="true">{entry.client_icon}</span>
              )
            ) : null}
            {entry.client_label}
          </button>
        )}
        <div style={{ flex: 1 }} />
        {blank ? (
          <button
            type="button"
            title={retrying ? "Retrying…" : "Retry transcription"}
            className={`voxix-icon-btn${retrying ? " spinning" : ""}`}
            onClick={retry}
            disabled={retrying}
            aria-busy={retrying}
          >
            <RefreshIcon size={14} />
          </button>
        ) : (
          <button
            type="button"
            title={
              copyState === "error"
                ? "Copy failed"
                : copyState === "success"
                  ? "Copied"
                  : "Copy to clipboard"
            }
            className="voxix-icon-btn"
            onClick={copy}
            style={
              copyState === "success"
                ? { color: "var(--success)" }
                : copyState === "error"
                  ? { color: "var(--danger)" }
                  : undefined
            }
            aria-live="polite"
          >
            {copyState === "success" ? (
              <CheckIcon size={14} />
            ) : copyState === "error" ? (
              <ExclamationTriangle size={14} />
            ) : (
              <CopyIcon size={14} />
            )}
          </button>
        )}
        <button
          type="button"
          title={playing ? "Pause audio" : "Play audio"}
          className="voxix-icon-btn"
          onClick={() => onTogglePlay(entry)}
          disabled={!entry.file_name}
          style={
            !entry.file_name
              ? { opacity: 0.4, pointerEvents: "none" }
              : undefined
          }
        >
          {playing ? (
            <span style={{ color: "var(--accent)" }}>
              <PauseIcon size={14} />
            </span>
          ) : (
            <PlayIcon size={14} />
          )}
        </button>
        <button
          type="button"
          title="Delete transcript"
          className="voxix-icon-btn danger"
          onClick={del}
        >
          <TrashIcon size={14} />
        </button>
      </div>
      {confirmDelete && (
        <div
          className="voxix-sheet-backdrop"
          role="dialog"
          aria-modal="true"
          onClick={() => setConfirmDelete(false)}
        >
          <div
            className="voxix-card"
            style={{
              padding: 16,
              width: 320,
              boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
            }}
            onClick={(ev) => ev.stopPropagation()}
          >
            <div style={{ fontSize: "var(--text-base)", fontWeight: 600 }}>
              {t("voxix.history.deleteTitle")}
            </div>
            <div className="voxix-cap" style={{ marginTop: 4 }}>
              {t("voxix.history.deleteBody")}
            </div>
            <div
              style={{
                display: "flex",
                justifyContent: "flex-end",
                gap: 8,
                marginTop: 12,
              }}
            >
              <Btn onClick={() => setConfirmDelete(false)}>
                {t("voxix.history.cancel")}
              </Btn>
              <Btn
                variant="destructive-primary"
                onClick={async () => {
                  setConfirmDelete(false);
                  try {
                    const res = await commands.deleteHistoryEntry(entry.id);
                    if (res.status === "error") {
                      toast.error("Failed to delete entry", {
                        description: String(res.error),
                      });
                      return;
                    }
                  } catch (e) {
                    toast.error("Failed to delete entry", {
                      description: e instanceof Error ? e.message : String(e),
                    });
                    return;
                  }
                  onDeleted(entry);
                }}
              >
                {t("voxix.history.delete")}
              </Btn>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};
