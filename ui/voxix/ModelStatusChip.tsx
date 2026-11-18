import { useModelStore } from "@/stores/modelStore";

// Compact "download in flight" chip for the sidebar footer.
// Only mounted-rendered while at least one model is in a download phase.
export const ModelStatusChip = () => {
  const id = useModelStore(
    (s) =>
      Object.keys(s.downloadingModels)[0] ??
      Object.keys(s.verifyingModels)[0] ??
      Object.keys(s.extractingModels)[0] ??
      null,
  );
  const phase = useModelStore((s) =>
    id && s.extractingModels[id]
      ? "Extracting…"
      : id && s.verifyingModels[id]
        ? "Verifying…"
        : id
          ? `Downloading ${Math.round(s.downloadProgress[id]?.percentage ?? 0)}%`
          : "",
  );
  if (!id || !phase) return null;
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 6,
        marginTop: "auto",
        padding: "6px 10px",
        fontSize: 11,
        color: "var(--text-dim)",
        overflow: "hidden",
      }}
    >
      <span
        style={{
          width: 6,
          height: 6,
          borderRadius: 3,
          background: "var(--accent)",
          flexShrink: 0,
          animation: "vx-blink 1.2s ease-in-out infinite",
        }}
      />
      <span
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {phase}
      </span>
    </div>
  );
};
