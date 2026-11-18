import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowDownCircle,
  CanaryIcon,
  CheckmarkCircle,
  Circle,
  CohereIcon,
  ExclamationTriangle,
  FunAsrIcon,
  GenericModelIcon,
  GigaIcon,
  GraniteIcon,
  LockShield,
  MoonshineIcon,
  NemotronIcon,
  ParakeetIcon,
  QwenIcon,
  VoxtralIcon,
  Waveform,
  WhisperIcon,
} from "./icons";
import { Badge, Btn, Dots } from "./ui";
import { ModelDownloadProgress } from "./ModelDownloadProgress";
import { useModelStore } from "@/stores/modelStore";
import type { ModelInfo } from "@/bindings";

const fmtSize = (mb: number) =>
  mb >= 1024 ? `${(mb / 1024).toFixed(1)}GB` : `${Math.round(mb)}MB`;

const providerIcon = (m: ModelInfo) => {
  const id = m.id.toLowerCase();
  const name = m.name.toLowerCase();
  const key = `${id} ${name}`;
  if (key.includes("parakeet")) return ParakeetIcon;
  if (key.includes("canary")) return CanaryIcon;
  if (key.includes("whisper")) return WhisperIcon;
  if (key.includes("voxtral")) return VoxtralIcon;
  if (key.includes("qwen")) return QwenIcon;
  if (key.includes("granite")) return GraniteIcon;
  if (key.includes("giga")) return GigaIcon;
  if (key.includes("moonshine")) return MoonshineIcon;
  if (key.includes("cohere")) return CohereIcon;
  if (key.includes("fun-asr") || key.includes("fun_asr")) return FunAsrIcon;
  if (key.includes("nemotron")) return NemotronIcon;
  if (key.includes("sense")) return Waveform;
  if (key.includes("breeze")) return Waveform;
  if (m.engine_type === "Parakeet") return ParakeetIcon;
  if (m.engine_type === "Canary") return CanaryIcon;
  if (m.engine_type === "Cohere") return CohereIcon;
  if (m.engine_type === "GigaAM") return GigaIcon;
  if (m.engine_type === "Moonshine" || m.engine_type === "MoonshineStreaming")
    return MoonshineIcon;
  if (m.engine_type === "SenseVoice") return Waveform;
  return GenericModelIcon;
};

const ProviderIcon = ({
  model,
  size = 18,
}: {
  model: ModelInfo;
  size?: number;
}) => {
  const Icon = providerIcon(model);
  return <Icon size={size} />;
};

const badge = (m: ModelInfo): string | null => {
  if (!m.is_recommended) return null;
  if (m.supported_languages.length <= 1) return "Best for English";
  return "Best for Multilingual";
};

const statusOf = (
  m: ModelInfo,
  current: string,
  downloading: boolean,
): "in-use" | "installed" | "downloading" | "none" => {
  if (downloading) return "downloading";
  if (m.is_downloaded && m.id === current) return "in-use";
  if (m.is_downloaded) return "installed";
  return "none";
};

export const ModelSection = () => {
  const { t } = useTranslation();
  const models = useModelStore((s) => s.models);
  const current = useModelStore((s) => s.currentModel);
  const initialize = useModelStore((s) => s.initialize);
  const initialized = useModelStore((s) => s.initialized);
  const [library, setLibrary] = useState(false);

  useEffect(() => {
    if (!initialized) initialize();
  }, [initialized, initialize]);

  const active = models.find((m) => m.id === current);
  const downloaded = models.filter((m) => m.is_downloaded);

  if (!active || !active.is_downloaded) {
    const suggested = models.filter((m) => m.is_recommended).slice(0, 3);
    return (
      <>
        <div
          className="voxix-card"
          style={{
            padding: 16,
            width: "100%",
            display: "flex",
            flexDirection: "column",
            gap: 12,
            boxSizing: "border-box",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <div
              style={{
                width: 28,
                display: "flex",
                justifyContent: "center",
                color: "var(--text-muted)",
                flexShrink: 0,
              }}
            >
              <ArrowDownCircle size={22} />
            </div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div
                style={{
                  fontSize: "var(--text-base)",
                  fontWeight: 500,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {t("voxix.modelSection.chooseModelTitle")}
              </div>
              <div
                className="voxix-cap"
                style={{
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {t("voxix.modelSection.chooseModelCaption")}
              </div>
            </div>
            <div style={{ flexShrink: 0, marginLeft: 8 }}>
              <Btn onClick={() => setLibrary(true)}>
                {t("voxix.modelSection.viewAllModels")}
              </Btn>
            </div>
          </div>
          {active && !active.is_downloaded && <MissingRow model={active} />}
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            {suggested.map((m) => (
              <SuggestedCard key={m.id} model={m} />
            ))}
          </div>
        </div>
        {library && <ModelLibrary onClose={() => setLibrary(false)} />}
      </>
    );
  }

  return (
    <>
      <div
        className="voxix-card"
        style={{
          padding: 16,
          width: "100%",
          display: "flex",
          flexDirection: "column",
          gap: 12,
          boxSizing: "border-box",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <div
            style={{
              width: 28,
              display: "flex",
              justifyContent: "center",
              color: "var(--accent)",
              flexShrink: 0,
            }}
          >
            <ProviderIcon model={active} size={22} />
          </div>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div
              style={{
                fontSize: "var(--text-base)",
                fontWeight: 500,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {active.name}
            </div>
            <div
              className="voxix-cap"
              style={{
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {active.supported_languages.length > 1
                ? "Multilingual"
                : "English"}{" "}
              · {fmtSize(active.size_mb)}
            </div>
          </div>
          <div style={{ flexShrink: 0, marginLeft: 8 }}>
            <Btn onClick={() => setLibrary(true)}>
              {t("voxix.modelSection.browseModels")}
            </Btn>
          </div>
        </div>
        {downloaded.length === 0 && (
          <div className="voxix-cap" style={{ marginTop: 0 }}>
            {t("voxix.modelSection.noModelsDownloaded")}
          </div>
        )}
      </div>
      {library && <ModelLibrary onClose={() => setLibrary(false)} />}
    </>
  );
};

const MissingRow = ({ model }: { model: ModelInfo }) => {
  const { t } = useTranslation();
  const downloadModel = useModelStore((s) => s.downloadModel);
  const downloading = useModelStore((s) => s.isModelDownloading(model.id));
  const progress = useModelStore((s) => s.getDownloadProgress(model.id));
  const cancel = useModelStore((s) => s.cancelDownload);

  return (
    <div
      style={{
        padding: 10,
        borderRadius: 8,
        border: "1px solid var(--border)",
        background: "rgba(255,159,10,0.06)",
        display: "flex",
        alignItems: "center",
        gap: 10,
      }}
    >
      <span
        style={{ color: "var(--voxix-orange)", display: "flex", flexShrink: 0 }}
      >
        <ExclamationTriangle size={18} />
      </span>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: "var(--text-base)", fontWeight: 500 }}>
          {model.name}
        </div>
        {downloading ? (
          <div style={{ marginTop: 4 }}>
            <div className="voxix-progress">
              <div style={{ width: `${progress?.percentage ?? 0}%` }} />
            </div>
            <div className="voxix-cap" style={{ marginTop: 4 }}>
              {t("voxix.modelSection.downloadingModel", { name: model.name })}
            </div>
          </div>
        ) : (
          <div className="voxix-cap" style={{ color: "var(--voxix-orange)" }}>
            {t("voxix.modelSection.notDownloadedWarning")}
          </div>
        )}
      </div>
      {downloading ? (
        <>
          <span className="voxix-cap tabular-nums">
            {Math.round(progress?.percentage ?? 0)}%
          </span>
          <Btn variant="destructive" onClick={() => cancel(model.id)}>
            {t("voxix.modelSection.cancel")}
          </Btn>
        </>
      ) : (
        <Btn variant="primary" onClick={() => downloadModel(model.id)}>
          {t("voxix.modelSection.download")}
        </Btn>
      )}
    </div>
  );
};

const SuggestedCard = ({ model }: { model: ModelInfo }) => {
  const { t } = useTranslation();
  const downloadModel = useModelStore((s) => s.downloadModel);
  const downloading = useModelStore((s) => !!s.downloadingModels[model.id]);
  const failed = useModelStore((s) => s.failedModels[model.id]);
  const erroring = failed !== undefined && !model.is_downloaded;

  return (
    <div
      className="voxix-card voxix-card-hover"
      data-recommended={model.is_recommended}
      style={{ flex: "1 1 160px", padding: 12, minWidth: 0 }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span
          style={{
            flex: 1,
            minWidth: 0,
            fontSize: "var(--text-base)",
            fontWeight: 500,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {model.name}
        </span>
        {model.is_recommended && (
          <Badge>{t("voxix.modelSection.suggested")}</Badge>
        )}
      </div>
      <div className="voxix-cap" style={{ marginTop: 4 }}>
        {model.supported_languages.length > 1 ? "Multilingual" : "English"} ·{" "}
        {fmtSize(model.size_mb)}
      </div>
      {downloading ? (
        <ModelDownloadProgress id={model.id} />
      ) : erroring ? (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 8,
            marginTop: 6,
          }}
        >
          <span
            style={{
              color: "var(--danger)",
              fontSize: 11,
              flex: 1,
              minWidth: 0,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {failed}
          </span>
          <Btn variant="primary" onClick={() => downloadModel(model.id)}>
            {t("voxix.modelSection.retry")}
          </Btn>
        </div>
      ) : (
        <div
          style={{ display: "flex", justifyContent: "flex-end", marginTop: 6 }}
        >
          <Btn variant="primary" onClick={() => downloadModel(model.id)}>
            {t("voxix.modelSection.download")}
          </Btn>
        </div>
      )}
    </div>
  );
};

const ModelLibrary = ({ onClose }: { onClose: () => void }) => {
  const { t } = useTranslation();
  const models = useModelStore((s) => s.models);
  const installed = models.filter((m) => m.is_downloaded);
  const recommended = models.filter(
    (m) => !m.is_downloaded && m.is_recommended,
  );
  const other = models.filter((m) => !m.is_downloaded && !m.is_recommended);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="voxix-sheet-backdrop"
      onClick={onClose}
      role="dialog"
      aria-modal="true"
    >
      <div className="voxix-sheet" onClick={(e) => e.stopPropagation()}>
        <div
          style={{
            padding: "16px 16px 12px",
            display: "flex",
            alignItems: "flex-start",
            gap: 12,
          }}
        >
          <div style={{ flex: 1, minWidth: 0 }}>
            <div
              style={{
                fontSize: "var(--text-2xl)",
                fontWeight: 600,
                lineHeight: "var(--leading-2xl)",
              }}
            >
              {t("voxix.modelSection.modelLibraryTitle")}
            </div>
            <div className="voxix-cap" style={{ marginTop: 2 }}>
              {t("voxix.modelSection.modelLibraryCaption")}
            </div>
          </div>
          <Btn variant="primary" onClick={onClose}>
            {t("voxix.modelSection.done")}
          </Btn>
        </div>
        <div
          style={{
            flex: 1,
            minHeight: 0,
            overflow: "auto",
            display: "flex",
            flexDirection: "column",
            gap: 16,
            padding: "0 16px 16px",
          }}
        >
          <LibraryGroup title="Installed" models={installed} />
          <LibraryGroup title="Recommended" models={recommended} />
          <LibraryGroup title="Other Models" models={other} />
          <div
            className="voxix-cap"
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              justifyContent: "center",
            }}
          >
            <LockShield size={12} />
            {t("voxix.modelSection.localModelsNote")}
          </div>
        </div>
      </div>
    </div>
  );
};

const LibraryGroup = ({
  title,
  models,
}: {
  title: string;
  models: ModelInfo[];
}) => {
  if (models.length === 0) return null;
  return (
    <div>
      <div
        className="voxix-cap"
        style={{ fontWeight: 600, margin: "0 0 6px 4px" }}
      >
        {title}
      </div>
      <div className="voxix-card">
        {models.map((m) => (
          <LibraryRow key={m.id} model={m} />
        ))}
      </div>
    </div>
  );
};

const QuantSelect = ({
  model,
  value,
  onChange,
}: {
  model: ModelInfo;
  value: string;
  onChange: (v: string) => void;
}) => (
  <select
    className="voxix-select"
    style={{ minWidth: 0, flexShrink: 0 }}
    value={value}
    onChange={(e) => onChange(e.target.value)}
    aria-label="Quantization"
  >
    {model.quant_files.map((f) => (
      <option key={f.filename} value={f.filename}>
        {f.quant} · {fmtSize(f.size_mb)}
      </option>
    ))}
  </select>
);

const LibraryRow = ({ model: m }: { model: ModelInfo }) => {
  const { t } = useTranslation();
  const current = useModelStore((s) => s.currentModel);
  const selectModel = useModelStore((s) => s.selectModel);
  const downloadModel = useModelStore((s) => s.downloadModel);
  const cancel = useModelStore((s) => s.cancelDownload);
  const deleteModel = useModelStore((s) => s.deleteModel);
  const repoId =
    typeof m.source === "object" && "HuggingFace" in m.source
      ? m.source.HuggingFace.repo_id
      : null;
  const hasQuants = m.quant_files.length > 1;
  const [quantFile, setQuantFile] = useState(m.filename);
  // Any listed quant downloads under its own registry id
  // ("{repo_id}/{filename}"), which the backend resolves from the catalog.
  const dlId = repoId && quantFile ? `${repoId}/${quantFile}` : m.id;
  const progress = useModelStore((s) => s.getDownloadProgress(dlId));
  const downloading = useModelStore((s) => !!s.downloadingModels[dlId]);
  const verifying = useModelStore((s) => !!s.verifyingModels[dlId]);
  const extracting = useModelStore((s) => !!s.extractingModels[dlId]);
  const failed = useModelStore((s) => s.failedModels[dlId]);
  const speed = useModelStore((s) => s.downloadStats[dlId]?.speed);
  const selectedQuant = m.quant_files.find((f) => f.filename === quantFile);
  const [confirming, setConfirming] = useState(false);
  const status = statusOf(m, current, downloading);
  const pct = Math.round(progress?.percentage ?? 0);
  const busy = downloading || verifying || extracting;
  const b = badge(m);

  return (
    <div
      className="voxix-row"
      data-selected={status === "in-use" ? "true" : undefined}
    >
      <div
        className="voxix-provider-icon"
        style={{
          width: 28,
          height: 28,
          borderRadius: 6,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          background: status === "in-use" ? "var(--accent)" : "var(--surface)",
          color: status === "in-use" ? "var(--bg)" : "var(--text-dim)",
          border: status === "in-use" ? "none" : "1px solid var(--border)",
          flexShrink: 0,
        }}
      >
        <ProviderIcon model={m} size={14} />
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span
            style={{
              fontSize: "var(--text-base)",
              fontWeight: 500,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {m.name}
          </span>
          {b && <Badge>{b}</Badge>}
          {status === "in-use" && (
            <span
              style={{
                width: 6,
                height: 6,
                borderRadius: 3,
                background: "var(--success)",
                flexShrink: 0,
              }}
            />
          )}
          {status === "downloading" && (
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
          )}
        </div>
        <div
          className="voxix-cap"
          style={{
            marginTop: 2,
            display: "flex",
            alignItems: "center",
            gap: 12,
          }}
        >
          <span>
            {m.supported_languages.length > 1 ? "Multilingual" : "English"}
          </span>
          {(m.accuracy_score !== 0 || m.speed_score !== 0) && (
            <>
              <span style={{ display: "flex", alignItems: "center", gap: 4 }}>
                {t("voxix.modelSection.accuracy")}{" "}
                <Dots value={Math.round(m.accuracy_score * 5)} />
              </span>
              <span style={{ display: "flex", alignItems: "center", gap: 4 }}>
                {t("voxix.modelSection.speed")}{" "}
                <Dots value={Math.round(m.speed_score * 5)} />
              </span>
            </>
          )}
        </div>
        {busy && (
          <div style={{ marginTop: 6 }}>
            <div className="voxix-progress">
              <div style={{ width: `${downloading ? pct : 100}%` }} />
            </div>
            <div className="voxix-cap tabular-nums" style={{ marginTop: 4 }}>
              {extracting && "Extracting…"}
              {verifying && !extracting && "Verifying…"}
              {downloading &&
                !verifying &&
                !extracting &&
                `Downloading… ${pct}%${speed != null ? ` · ${speed.toFixed(1)} MB/s` : ""}`}
            </div>
          </div>
        )}
      </div>
      <div style={{ textAlign: "right", marginRight: 8, flexShrink: 0 }}>
        <div className="voxix-cap">
          {fmtSize(selectedQuant ? selectedQuant.size_mb : m.size_mb)}
        </div>
        <div
          style={{
            fontSize: "var(--text-xs)",
            fontWeight: 500,
            color: status === "in-use" ? "var(--accent)" : "var(--text-dim)",
          }}
        >
          {status === "in-use" && "In Use"}
          {status === "installed" && "Installed"}
          {busy &&
            (extracting
              ? "Extracting…"
              : verifying
                ? "Verifying…"
                : "Downloading…")}
        </div>
      </div>
      {status === "downloading" && (
        <Btn variant="destructive" onClick={() => cancel(dlId)}>
          {t("voxix.modelSection.cancel")}
        </Btn>
      )}
      {status === "none" &&
        (failed !== undefined ? (
          <>
            {hasQuants && (
              <QuantSelect
                model={m}
                value={quantFile}
                onChange={setQuantFile}
              />
            )}
            <Btn variant="primary" onClick={() => downloadModel(dlId)}>
              {t("voxix.modelSection.retry")}
            </Btn>
          </>
        ) : (
          <>
            {hasQuants && (
              <QuantSelect
                model={m}
                value={quantFile}
                onChange={setQuantFile}
              />
            )}
            <Btn
              variant="primary"
              onClick={() => !downloading && downloadModel(dlId)}
            >
              {t("voxix.modelSection.download")}
            </Btn>
          </>
        ))}
      {status === "installed" && (
        <>
          <Btn onClick={() => selectModel(m.id)}>
            {t("voxix.modelSection.use")}
          </Btn>
          <Btn variant="destructive" onClick={() => setConfirming(true)}>
            {t("voxix.modelSection.remove")}
          </Btn>
        </>
      )}
      {status === "in-use" && (
        <Btn variant="destructive" onClick={() => setConfirming(true)}>
          {t("voxix.modelSection.remove")}
        </Btn>
      )}
      {confirming && (
        <div
          className="voxix-sheet-backdrop"
          onClick={() => setConfirming(false)}
        >
          <div
            className="voxix-card"
            style={{
              padding: 16,
              width: 300,
              boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <div style={{ fontSize: "var(--text-base)", fontWeight: 600 }}>
              {t("voxix.modelSection.removeModelTitle", { name: m.name })}
            </div>
            <div className="voxix-cap" style={{ marginTop: 4 }}>
              {t("voxix.modelSection.removeModelBody", {
                size: fmtSize(m.size_mb),
              })}
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
                {t("voxix.modelSection.cancel")}
              </Btn>
              <Btn
                variant="destructive-primary"
                onClick={async () => {
                  setConfirming(false);
                  await deleteModel(m.id);
                }}
              >
                {t("voxix.modelSection.removeDownload")}
              </Btn>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};
