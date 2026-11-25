import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast, Toaster } from "sonner";
import { commands } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { useModelStore } from "@/stores/modelStore";
import { useTranslation } from "react-i18next";
import { SettingsTab } from "./SettingsTab";
import { TransformsTab } from "./TransformsTab";
import { HistoryTab } from "./HistoryTab";
import { ServiceTab } from "./ServiceTab";
import { AboutTab } from "./AboutTab";
import {
  ClockIcon,
  GearIcon,
  InfoIcon,
  MenuIcon,
  ServerIcon,
  TransformsIcon,
  XIcon,
} from "./icons";
import { VxLogo } from "./VxLogo";
import { ModelStatusChip } from "./ModelStatusChip";
import { useEscape } from "./ui";
import "@fontsource-variable/inter";
import "./theme.css";

const _origToastError = toast.error;
(toast as unknown as { error: typeof toast.error }).error = (
  msg: Parameters<typeof toast.error>[0],
  opts?: Parameters<typeof toast.error>[1],
) =>
  _origToastError(
    msg as never,
    { id: typeof msg === "string" ? msg : String(msg), ...opts } as never,
  ) as never;

type Tab = "settings" | "transforms" | "history" | "service" | "about";

const retryFailedTranscription = async (entryId: number) => {
  const res = await commands.retryHistoryEntryTranscription(entryId);
  if (res.status === "ok") {
    toast.success("Transcription retried", { id: `retry-${entryId}` });
  } else {
    toast.error("Retry failed", {
      id: `retry-${entryId}`,
      description: String(res.error),
    });
  }
};

const TABS: {
  id: Tab;
  labelKey?: string;
  label?: string;
  icon: typeof GearIcon;
}[] = [
  { id: "settings", label: "Settings", icon: GearIcon },
  { id: "transforms", label: "Transforms", icon: TransformsIcon },
  { id: "history", label: "History", icon: ClockIcon },
  { id: "service", labelKey: "serviceTab.title", icon: ServerIcon },
  { id: "about", label: "About", icon: InfoIcon },
];

export const VoxixApp = () => {
  const { t: translate } = useTranslation();
  const [tab, setTab] = useState<Tab>("settings");
  const tabLabel = (entry: (typeof TABS)[number]) =>
    entry.labelKey ? translate(entry.labelKey) : (entry.label ?? entry.id);

  const [isSmallScreen, setIsSmallScreen] = useState(
    typeof window !== "undefined" ? window.innerWidth <= 700 : false,
  );
  const [sidebarExpanded, setSidebarExpanded] = useState(false);

  useEscape(isSmallScreen && sidebarExpanded, () => setSidebarExpanded(false));

  const initSettings = useSettingsStore((s) => s.initialize);
  const refreshAudio = useSettingsStore((s) => s.refreshAudioDevices);
  const refreshOutput = useSettingsStore((s) => s.refreshOutputDevices);
  const initModels = useModelStore((s) => s.initialize);

  useEffect(() => {
    initSettings();
    initModels();
    Promise.all([commands.initializeEnigo(), commands.initializeShortcuts()])
      .then(async ([enigoRes, shortcutsRes]) => {
        const failures: string[] = [];
        if (enigoRes.status === "error")
          failures.push(`input system: ${enigoRes.error}`);
        if (shortcutsRes.status === "error")
          failures.push(`shortcuts: ${shortcutsRes.error}`);
        if (failures.length > 0) {
          toast.error("Shortcuts failed to initialize", {
            id: "initialize-shortcuts",
            description: failures.join("; "),
          });
          // Do not mark onboarding complete; a later launch/retry re-attempts.
          return;
        }
        const res = await commands.setSettingValue(
          "onboarding_completed",
          "true",
        );
        if (res.status === "error") {
          toast.error("Failed to save onboarding state", {
            id: "onboarding-completed",
            description: String(res.error),
          });
        }
      })
      .catch((e) => {
        console.warn("Failed to initialize:", e);
        toast.error("Shortcuts failed to initialize", {
          id: "initialize-shortcuts",
          description: e instanceof Error ? e.message : String(e),
        });
      });
    refreshAudio();
    refreshOutput();
  }, [initSettings, initModels, refreshAudio, refreshOutput]);

  useEffect(() => {
    const unError = listen<{
      error: string;
      entry_id?: number | null;
      file_name?: string | null;
    }>("transcription-error", (e) => {
      const entryId = e.payload?.entry_id;
      toast.error("Transcription failed", {
        id: "transcription-error",
        description: e.payload?.error,
        action:
          entryId != null
            ? {
                label: "Retry",
                onClick: () => void retryFailedTranscription(entryId),
              }
            : { label: "History", onClick: () => setTab("history") },
      });
    });
    const unPaste = listen<string>("paste-error", (e) =>
      toast.error("Paste failed", {
        id: "paste-error",
        description:
          typeof e.payload === "string" && e.payload.trim().length > 0
            ? e.payload
            : "Could not paste the transcription into the active app.",
      }),
    );
    const unRecording = listen<{ error_type: string; detail?: string }>(
      "recording-error",
      (e) =>
        toast.error("Recording failed", {
          id: "recording-error",
          description: e.payload.detail ?? e.payload.error_type,
        }),
    );
    const unModel = listen<{
      event_type: string;
      model_name?: string;
      error?: string;
    }>("model-state-changed", (e) => {
      if (e.payload.event_type === "loading_failed")
        toast.error(`Failed to load ${e.payload.model_name ?? "model"}`, {
          id: `model-load-${e.payload.model_name ?? "unknown"}`,
          description: e.payload.error,
        });
    });
    return () => {
      [unError, unPaste, unRecording, unModel].forEach((u) =>
        u.then((f) => f()),
      );
    };
  }, []);

  useEffect(() => {
    const mql = window.matchMedia("(max-width: 700px)");
    const handler = (e: MediaQueryListEvent) => {
      setIsSmallScreen(e.matches);
      if (!e.matches) setSidebarExpanded(false);
    };
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, []);

  const collapsed = isSmallScreen && !sidebarExpanded;

  return (
    <div
      className="voxix-root"
      style={{
        display: "flex",
        flexDirection: "row",
        width: "100%",
        height: "100vh",
        overflow: "hidden",
        margin: 0,
        padding: 0,
        boxSizing: "border-box",
      }}
    >
      <Toaster
        theme="dark"
        toastOptions={{
          style: {
            background: "var(--surface)",
            border: "1px solid var(--border)",
            color: "var(--text)",
          },
        }}
      />
      {isSmallScreen && sidebarExpanded && (
        <div
          className="voxix-sidebar-backdrop"
          onClick={() => setSidebarExpanded(false)}
          aria-hidden="true"
        />
      )}
      <nav
        className="voxix-sidebar"
        data-collapsed={collapsed ? "true" : undefined}
        data-drawer={isSmallScreen && sidebarExpanded ? "true" : undefined}
        style={{
          width: collapsed ? "56px" : "var(--sidebar-width)",
          minWidth: collapsed ? "56px" : "var(--sidebar-width)",
          flexShrink: 0,
          display: "flex",
          flexDirection: "column",
          padding: collapsed ? "12px 6px 8px" : "12px 8px 8px",
          gap: 2,
          borderRight: "1px solid var(--border)",
          background: "var(--bg-elev)",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: collapsed ? "center" : "flex-start",
            gap: 8,
            padding: collapsed ? "8px 0 12px" : "8px 8px 12px",
            width: "100%",
          }}
        >
          {collapsed ? (
            <button
              type="button"
              className="voxix-icon-btn"
              onClick={() => setSidebarExpanded(true)}
              aria-label="Expand sidebar"
              title="Expand sidebar"
              style={{ padding: 6 }}
            >
              <MenuIcon size={18} />
            </button>
          ) : (
            <>
              <VxLogo size={20} />
              <span
                className="voxix-sidebar-title"
                style={{
                  fontSize: "var(--text-md)",
                  fontWeight: 600,
                  letterSpacing: "var(--tracking-tight)",
                  flex: 1,
                }}
              >
                {translate("voxix.voxixApp.voxix")}
              </span>
              {isSmallScreen && sidebarExpanded && (
                <button
                  type="button"
                  className="voxix-icon-btn"
                  onClick={() => setSidebarExpanded(false)}
                  aria-label="Collapse sidebar"
                  title="Collapse sidebar"
                  style={{ padding: 6 }}
                >
                  <XIcon size={16} />
                </button>
              )}
            </>
          )}
        </div>
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            className="voxix-sidebar-item"
            data-active={tab === t.id}
            title={tabLabel(t)}
            onClick={() => {
              setTab(t.id);
              if (isSmallScreen && sidebarExpanded) setSidebarExpanded(false);
            }}
          >
            <t.icon size={16} />
            {!collapsed && (
              <span className="voxix-sidebar-label">{tabLabel(t)}</span>
            )}
          </button>
        ))}
        <ModelStatusChip />
      </nav>
      <main
        style={{
          flex: 1,
          minWidth: 0,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            flex: 1,
            overflow: "auto",
            minWidth: 0,
            minHeight: 0,
            scrollbarGutter: "stable" as const,
          }}
        >
          <div
            className="voxix-content-wrap"
            style={{
              display: "flex",
              flexDirection: "column",
              alignItems: "safe center",
              boxSizing: "border-box",
              width: "100%",
            }}
          >
            {tab === "settings" && <SettingsTab />}
            {tab === "transforms" && <TransformsTab />}
            {tab === "history" && <HistoryTab />}
            {tab === "service" && <ServiceTab />}
            {tab === "about" && <AboutTab />}
          </div>
        </div>
      </main>
    </div>
  );
};
