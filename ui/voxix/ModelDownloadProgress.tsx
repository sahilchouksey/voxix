import { useModelStore } from "@/stores/modelStore";
import { useTranslation } from "react-i18next";
import { Btn } from "./ui";

// Reactive per-model download state machine UI.
// Re-renders on every model-download-progress event (~10 Hz) because all
// state reads are zustand selectors — never use getState() in render.
export const ModelDownloadProgress = ({ id }: { id: string }) => {
  const { t } = useTranslation();
  const downloading = useModelStore((s) => !!s.downloadingModels[id]);
  const verifying = useModelStore((s) => !!s.verifyingModels[id]);
  const extracting = useModelStore((s) => !!s.extractingModels[id]);
  const progress = useModelStore((s) => s.downloadProgress[id]);
  const speed = useModelStore((s) => s.downloadStats[id]?.speed);
  const cancel = useModelStore((s) => s.cancelDownload);

  const phase = extracting
    ? "extracting"
    : verifying
      ? "verifying"
      : downloading
        ? "downloading"
        : null;
  if (!phase) return null;

  const pct = Math.round(progress?.percentage ?? 0);

  return (
    <div style={{ marginTop: 4 }}>
      <div className="voxix-progress">
        <div style={{ width: `${phase === "downloading" ? pct : 100}%` }} />
      </div>
      <div
        className="voxix-cap"
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
          marginTop: 4,
        }}
      >
        <span>
          {phase === "downloading" && `Downloading… ${pct}%`}
          {phase === "verifying" && "Verifying…"}
          {phase === "extracting" && "Extracting…"}
        </span>
        <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
          {phase === "downloading" && speed != null && (
            <span className="tabular-nums">
              {t("voxix.modelDownloadProgress.speedValue", {
                speed: speed.toFixed(1),
              })}
            </span>
          )}
          {phase === "downloading" && (
            <Btn variant="destructive" onClick={() => cancel(id)}>
              {t("voxix.modelDownloadProgress.cancel")}
            </Btn>
          )}
        </span>
      </div>
    </div>
  );
};
