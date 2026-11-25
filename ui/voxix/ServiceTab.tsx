import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useServiceStore } from "@/stores/serviceStore";
import {
  Badge,
  Btn,
  Chip,
  Group,
  Row,
  Screen,
  Segmented,
  Stat,
  StatGrid,
  useEscape,
} from "./ui";
import {
  CheckIcon,
  CodeIcon,
  CopyIcon,
  CpuIcon,
  EyeIcon,
  EyeOffIcon,
  ExclamationTriangle,
  LinkIcon,
  LockShield,
  RefreshIcon,
  ServerIcon,
  Waveform,
  XmarkCircle,
} from "./icons";

const EXAMPLES_URL =
  "https://github.com/sahilchouksey/voxix/tree/main/examples";

const MONO = { fontFamily: "var(--font-mono)" } as const;

const truncateId = (id: string) =>
  id.length > 16 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;

/**
 * Compact human duration using the two largest non-zero units, e.g.
 * `21133` → `5h 52m`, `90061` → `1d 1h`, `40000000` → `1y 3mo`.
 */
const formatUptime = (totalSeconds: number): string => {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return "—";
  const s = Math.floor(totalSeconds);
  if (s < 60) return `${s}s`;
  const units: Array<[string, number]> = [
    ["y", 365 * 86400],
    ["mo", 30 * 86400],
    ["d", 86400],
    ["h", 3600],
    ["m", 60],
    ["s", 1],
  ];
  const parts: string[] = [];
  let rest = s;
  for (const [label, size] of units) {
    const n = Math.floor(rest / size);
    if (n > 0) {
      parts.push(`${n}${label}`);
      rest -= n * size;
      if (parts.length === 2) break;
    }
  }
  return parts.join(" ");
};

const probeErrorKey = (code: string | null | undefined): string | null => {
  if (!code) return null;
  switch (code) {
    case "no_token":
      return "serviceTab.errors.noToken";
    case "unauthorized":
      return "serviceTab.errors.unauthorized";
    case "unreachable":
      return "serviceTab.errors.unreachable";
    case "malformed_response":
      return "serviceTab.errors.malformed";
    default:
      return null;
  }
};

export const ServiceTab = () => {
  const { t } = useTranslation();
  const {
    discovery,
    probe,
    tailscale,
    loading,
    starting,
    stopping,
    rotating,
    error,
    rotatedToken,
    serviceToken,
    refresh,
    start,
    stop,
    rotateToken,
    dismissRotatedToken,
    clearError,
  } = useServiceStore();

  type HostMode = "local" | "ip" | "dns";
  const [hostMode, setHostMode] = useState<HostMode>("local");

  const [confirmStop, setConfirmStop] = useState(false);
  const [confirmRotate, setConfirmRotate] = useState(false);
  const [showFingerprint, setShowFingerprint] = useState(false);
  const [showServiceToken, setShowServiceToken] = useState(false);
  const [showConnectionUrl, setShowConnectionUrl] = useState(false);
  const [baseCopied, setBaseCopied] = useState(false);
  const [connectionCopied, setConnectionCopied] = useState(false);
  const [tokenCopied, setTokenCopied] = useState(false);
  const [copied, setCopied] = useState(false);

  useEscape(confirmStop, () => setConfirmStop(false));
  useEscape(confirmRotate, () => setConfirmRotate(false));

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    setCopied(false);
  }, [rotatedToken]);

  const selectedHost =
    tailscale?.available && hostMode === "ip" && tailscale.ip
      ? tailscale.ip
      : tailscale?.available && hostMode === "dns" && tailscale.dns
        ? tailscale.dns
        : "127.0.0.1";

  const baseUrl =
    discovery?.port != null ? `http://${selectedHost}:${discovery.port}` : null;
  const connectionUrl =
    discovery?.port != null && serviceToken
      ? `http://${selectedHost}:${discovery.port}?api_key=${serviceToken}`
      : null;
  const running =
    !!discovery?.present && discovery.pid_alive && discovery.port_open;
  const stale = !!discovery?.present && !running;
  const statusLabel = starting
    ? t("serviceTab.daemon.starting")
    : stopping
      ? t("serviceTab.daemon.stopping")
      : running
        ? t("serviceTab.daemon.running")
        : stale
          ? t("serviceTab.daemon.stale")
          : t("serviceTab.daemon.stopped");
  const statusColor = running
    ? "var(--success)"
    : stale
      ? "var(--voxix-orange)"
      : "var(--text-muted)";

  const health = probe?.health ?? null;
  const healthError = probe?.health_error ?? null;
  const status = probe?.status ?? null;
  const statusError = probe?.status_error ?? null;

  const errorKey = probeErrorKey(healthError ?? statusError);

  const uptimeSeconds = health ? Math.round(health.uptime_ms / 1000) : null;

  const copyRotatedToken = async () => {
    if (!rotatedToken) return;
    try {
      await writeText(rotatedToken.token);
      setCopied(true);
    } catch {
      toast.error(t("serviceTab.token.copyFailed"));
    }
  };

  const copyServiceToken = async () => {
    if (!serviceToken) return;
    try {
      await writeText(serviceToken);
      setTokenCopied(true);
      setTimeout(() => setTokenCopied(false), 1500);
    } catch {
      toast.error(t("serviceTab.token.copyFailed"));
    }
  };

  const copyBaseUrl = async () => {
    if (!baseUrl) return;
    try {
      await writeText(baseUrl);
      setBaseCopied(true);
      setTimeout(() => setBaseCopied(false), 1500);
    } catch {
      toast.error(t("serviceTab.token.copyFailed"));
    }
  };

  const copyConnectionUrlFn = async () => {
    if (!connectionUrl) return;
    try {
      await writeText(connectionUrl);
      setConnectionCopied(true);
      setTimeout(() => setConnectionCopied(false), 1500);
    } catch {
      toast.error(t("serviceTab.token.copyFailed"));
    }
  };

  const confirmStopNow = async () => {
    setConfirmStop(false);
    const stopped = await stop();
    if (stopped) {
      toast.success(t("serviceTab.daemon.stoppedToast"));
    }
  };

  const confirmRotateNow = async () => {
    setConfirmRotate(false);
    const ok = await rotateToken();
    if (ok) {
      toast.success(t("serviceTab.token.rotatedToast"));
    }
  };

  return (
    <Screen title={t("serviceTab.title")} caption={t("serviceTab.caption")}>
      {error && (
        <div
          className="voxix-row"
          style={{
            alignItems: "flex-start",
            gap: 8,
            border: "1px solid var(--danger)",
            borderRadius: 8,
            padding: 12,
          }}
          role="alert"
        >
          <ExclamationTriangle size={16} />
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontSize: "var(--text-base)" }}>
              {t("serviceTab.errors.unknown")}
            </div>
            <div
              className="voxix-cap"
              style={{ ...MONO, marginTop: 2, overflowWrap: "anywhere" }}
            >
              {error}
            </div>
          </div>
          <button
            type="button"
            className="voxix-icon-btn"
            aria-label={t("common.close")}
            onClick={clearError}
          >
            <XmarkCircle size={16} />
          </button>
        </div>
      )}

      {/* ── Daemon ── */}
      <Group
        title={t("serviceTab.daemon.groupTitle")}
        icon={<ServerIcon size={14} />}
      >
        {/* Hero status row */}
        <div className="voxix-row" style={{ gap: 8 }}>
          <span
            aria-hidden="true"
            style={{
              width: 8,
              height: 8,
              borderRadius: "50%",
              background: statusColor,
              display: "inline-block",
              flexShrink: 0,
            }}
          />
          <div style={{ flex: 1, minWidth: 0 }}>
            <div
              style={{
                fontSize: "var(--text-base)",
                fontWeight: "var(--weight-semibold)",
              }}
            >
              {statusLabel}
            </div>
          </div>
          <button
            type="button"
            className="voxix-icon-btn"
            aria-label={t("serviceTab.daemon.refresh")}
            onClick={() => void refresh()}
            disabled={loading}
          >
            <RefreshIcon size={16} />
          </button>
          <Btn
            onClick={async () => {
              const result = await start();
              if (result.ok) {
                toast.success(
                  result.started
                    ? t("serviceTab.daemon.startedToast")
                    : t("serviceTab.daemon.alreadyRunningToast"),
                );
              }
            }}
            disabled={starting || running}
          >
            {starting
              ? t("serviceTab.daemon.starting")
              : t("serviceTab.daemon.start")}
          </Btn>
          <Btn
            variant="destructive"
            onClick={() => setConfirmStop(true)}
            disabled={stopping || !discovery?.present || !discovery.pid_alive}
          >
            {stopping
              ? t("serviceTab.daemon.stopping")
              : t("serviceTab.daemon.stop")}
          </Btn>
        </div>

        {/* Stat strip: Port / PID / Uptime */}
        <StatGrid>
          <Stat
            label={t("serviceTab.daemon.port")}
            value={discovery?.port ?? "—"}
          />
          <Stat
            label={t("serviceTab.daemon.processId")}
            value={discovery?.pid ?? "—"}
          />
          <Stat
            label={t("serviceTab.daemon.uptime")}
            value={uptimeSeconds != null ? formatUptime(uptimeSeconds) : "—"}
          />
        </StatGrid>

        {/* Engine version */}
        {health && (
          <Row
            title={t("serviceTab.daemon.engineVersion")}
            control={
              <span className="voxix-cap" style={MONO}>
                {health.engine_version}
              </span>
            }
          />
        )}

        {/* Protocol version */}
        {discovery?.present && discovery.protocol_version != null && (
          <Row
            title={t("serviceTab.daemon.protocolVersion")}
            control={
              <span className="voxix-cap" style={MONO}>
                {discovery.protocol_version}
              </span>
            }
          />
        )}

        {/* Ready badge */}
        {health && (
          <Row
            title={t("serviceTab.daemon.ready")}
            control={
              <Badge>
                {health.ready
                  ? t("serviceTab.daemon.ready")
                  : t("serviceTab.daemon.notReady")}
              </Badge>
            }
          />
        )}

        {/* Probe errors */}
        {(healthError || statusError) && (
          <Row
            title={t("serviceTab.errors.unknown")}
            control={
              <span className="voxix-cap" style={MONO} role="status">
                {errorKey ? t(errorKey) : (healthError ?? statusError)}
              </span>
            }
          />
        )}
      </Group>

      {/* ── Engine ── */}
      {(status?.active_model_id ||
        status?.backend ||
        status?.capabilities) && (
        <Group
          title={t("serviceTab.engine.groupTitle")}
          icon={<CpuIcon size={14} />}
        >
          {/* Active model */}
          <Row
            title={t("serviceTab.engine.activeModel")}
            stack
            control={
              status?.active_model_id ? (
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 8,
                    flexWrap: "wrap",
                  }}
                >
                  <span className="voxix-cap" style={MONO}>
                    {status.active_model_id}
                  </span>
                  <Badge>{status.model_state}</Badge>
                </div>
              ) : (
                <span className="voxix-cap">
                  {t("serviceTab.engine.noModel")}
                </span>
              )
            }
          />

          {/* Backend */}
          {status?.backend && (
            <Row
              title={t("serviceTab.engine.backend")}
              control={
                <span className="voxix-cap" style={MONO}>
                  {status.backend}
                </span>
              }
            />
          )}

          {/* Capabilities */}
          {status?.capabilities && (
            <Row
              title={t("serviceTab.engine.capabilities")}
              stack
              control={
                <div className="voxix-chip-row">
                  {(() => {
                    const caps = status.capabilities;
                    const n = caps.max_concurrent_streams;
                    return (
                      <>
                        <Chip>{`${n} stream${n === 1 ? "" : "s"}`}</Chip>
                        <Chip>{`VAD ${caps.requires_server_vad ? "server" : "client"}`}</Chip>
                        <Chip>{`Gain ${caps.prefers_auto_gain ? "auto" : "off"}`}</Chip>
                        <Chip>{`Denoise ${caps.prefers_noise_reduction ? "on" : "off"}`}</Chip>
                      </>
                    );
                  })()}
                </div>
              }
            />
          )}
        </Group>
      )}

      {/* ── Connection ── */}
      {(baseUrl || connectionUrl || discovery?.token_fingerprint) && (
        <Group
          title={t("serviceTab.connection.groupTitle")}
          icon={<LinkIcon size={14} />}
        >
          {/* Base URL */}
          {baseUrl && (
            <Row
              title={t("serviceTab.connection.baseUrl")}
              stack
              control={
                <div
                  style={{ display: "flex", alignItems: "center", gap: 8 }}
                >
                  <code
                    style={{
                      ...MONO,
                      fontSize: "var(--text-sm)",
                      flex: 1,
                      minWidth: 0,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {baseUrl}
                  </code>
                  <button
                    type="button"
                    className="voxix-icon-btn"
                    aria-label={t("serviceTab.connection.copyBaseUrl")}
                    onClick={() => void copyBaseUrl()}
                  >
                    {baseCopied ? (
                      <CheckIcon size={14} />
                    ) : (
                      <CopyIcon size={14} />
                    )}
                  </button>
                </div>
              }
            />
          )}

          {/* URL with token */}
          {connectionUrl && (
            <Row
              title={
                tailscale?.available && (tailscale.ip || tailscale.dns) ? (
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "space-between",
                      gap: 8,
                      width: "100%",
                    }}
                  >
                    <span>{t("serviceTab.connection.withToken")}</span>
                    <Segmented
                      options={[
                        { value: "local", label: "Local" },
                        ...(tailscale.ip ? [{ value: "ip", label: "Tailscale IP" }] : []),
                        ...(tailscale.dns ? [{ value: "dns", label: "MagicDNS" }] : []),
                      ]}
                      value={hostMode}
                      onChange={(v) => setHostMode(v as HostMode)}
                    />
                  </div>
                ) : (
                  t("serviceTab.connection.withToken")
                )
              }
              stack
              control={
                <div
                  style={{ display: "flex", alignItems: "center", gap: 8 }}
                >
                  <code
                    style={{
                      ...MONO,
                      fontSize: "var(--text-sm)",
                      flex: 1,
                      minWidth: 0,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {showConnectionUrl
                      ? connectionUrl
                      : connectionUrl.replace(/api_key=.*/, "api_key=••••••••")}
                  </code>
                  <button
                    type="button"
                    className="voxix-icon-btn"
                    aria-label={
                      showConnectionUrl
                        ? t("serviceTab.connection.hideUrl")
                        : t("serviceTab.connection.showUrl")
                    }
                    onClick={() => setShowConnectionUrl((v) => !v)}
                  >
                    {showConnectionUrl ? (
                      <EyeOffIcon size={14} />
                    ) : (
                      <EyeIcon size={14} />
                    )}
                  </button>
                  <button
                    type="button"
                    className="voxix-icon-btn"
                    aria-label={t("serviceTab.connection.copyUrlWithToken")}
                    onClick={() => void copyConnectionUrlFn()}
                  >
                    {connectionCopied ? (
                      <CheckIcon size={14} />
                    ) : (
                      <CopyIcon size={14} />
                    )}
                  </button>
                </div>
              }
            />
          )}

          {/* Token fingerprint */}
          {discovery?.token_fingerprint && (
            <Row
              title={t("serviceTab.fingerprint.label")}
              control={
                <div
                  style={{ display: "flex", alignItems: "center", gap: 8 }}
                >
                  <span className="voxix-cap" style={MONO}>
                    {showFingerprint
                      ? discovery.token_fingerprint
                      : "••••••••••••••••"}
                  </span>
                  <button
                    type="button"
                    className="voxix-icon-btn"
                    aria-label={
                      showFingerprint
                        ? t("serviceTab.fingerprint.hide")
                        : t("serviceTab.fingerprint.reveal")
                    }
                    onClick={() => setShowFingerprint((v) => !v)}
                  >
                    {showFingerprint ? (
                      <EyeOffIcon size={14} />
                    ) : (
                      <EyeIcon size={14} />
                    )}
                  </button>
                </div>
              }
            />
          )}
        </Group>
      )}

      {/* ── Access token ── */}
      <Group
        title={t("serviceTab.token.groupTitle")}
        icon={<LockShield size={14} />}
      >
        <Row
          icon={<LockShield size={16} />}
          title={t("serviceTab.token.serviceTokenLabel")}
          control={
            serviceToken ? (
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <span className="voxix-cap" style={MONO}>
                  {showServiceToken
                    ? serviceToken
                    : "••••••••••••••••••••"}
                </span>
                <button
                  type="button"
                  className="voxix-icon-btn"
                  aria-label={
                    showServiceToken
                      ? t("serviceTab.token.serviceTokenHide")
                      : t("serviceTab.token.serviceTokenShow")
                  }
                  onClick={() => setShowServiceToken((v) => !v)}
                >
                  {showServiceToken ? (
                    <EyeOffIcon size={14} />
                  ) : (
                    <EyeIcon size={14} />
                  )}
                </button>
                <button
                  type="button"
                  className="voxix-icon-btn"
                  aria-label={t("serviceTab.token.copyToken")}
                  onClick={() => void copyServiceToken()}
                >
                  {tokenCopied ? (
                    <CheckIcon size={14} />
                  ) : (
                    <CopyIcon size={14} />
                  )}
                </button>
              </div>
            ) : (
              <span className="voxix-cap">
                {t("serviceTab.token.noTokenYet")}
              </span>
            )
          }
        />
        <Row
          title={t("serviceTab.token.rotate")}
          control={
            <Btn
              onClick={() => setConfirmRotate(true)}
              disabled={rotating || !serviceToken}
            >
              {rotating
                ? t("serviceTab.token.rotating")
                : t("serviceTab.token.rotate")}
            </Btn>
          }
        />
        {rotatedToken && (
          <div
            className="voxix-row"
            style={{ flexDirection: "column", alignItems: "stretch", gap: 8 }}
          >
            <div style={{ fontSize: "var(--text-base)", fontWeight: 600 }}>
              {t("serviceTab.token.newTokenTitle")}
            </div>
            <div className="voxix-cap">
              {t("serviceTab.token.newTokenBody")}
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <code
                style={{
                  ...MONO,
                  flex: 1,
                  minWidth: 0,
                  overflow: "auto",
                  whiteSpace: "nowrap",
                  fontSize: "var(--text-sm)",
                  padding: "4px 8px",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  background: "var(--surface)",
                }}
              >
                {rotatedToken.token}
              </code>
              <Btn onClick={copyRotatedToken}>
                {copied
                  ? t("serviceTab.token.copied")
                  : t("serviceTab.token.copy")}
              </Btn>
              <Btn onClick={dismissRotatedToken}>
                {t("serviceTab.token.dismiss")}
              </Btn>
            </div>
          </div>
        )}
      </Group>

      {/* ── Open sessions ── */}
      <Group
        title={t("serviceTab.sessions.groupTitle")}
        icon={<Waveform size={14} />}
      >
        <StatGrid>
          <Stat
            label={t("serviceTab.sessions.openSessions")}
            value={status?.active_sessions ?? 0}
          />
          <Stat
            label={t("serviceTab.sessions.leaseHolder")}
            value={
              status?.lease_held && status.active_session_id
                ? truncateId(status.active_session_id)
                : "—"
            }
          />
          <Stat
            label={t("serviceTab.sessions.realtimeFactor")}
            value={
              status?.max_realtime_factor != null
                ? status.max_realtime_factor.toFixed(2)
                : "—"
            }
          />
        </StatGrid>
      </Group>

      {/* ── Use this service ── */}
      <Group
        title={t("serviceTab.examples.groupTitle")}
        icon={<CodeIcon size={14} />}
      >
        <Row
          title={t("serviceTab.examples.repoLinkLabel")}
          control={
            <Btn onClick={() => void openUrl(EXAMPLES_URL)}>
              {t("serviceTab.examples.repoLinkAction")}
            </Btn>
          }
        />
      </Group>

      {confirmStop && (
        <div
          className="voxix-sheet-backdrop"
          role="dialog"
          aria-modal="true"
          onClick={() => setConfirmStop(false)}
        >
          <div
            className="voxix-card"
            style={{
              padding: 16,
              width: 360,
              boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
            }}
            onClick={(ev) => ev.stopPropagation()}
          >
            <div style={{ fontSize: "var(--text-base)", fontWeight: 600 }}>
              {t("serviceTab.daemon.stopConfirmTitle")}
            </div>
            <div className="voxix-cap" style={{ marginTop: 4 }}>
              {t("serviceTab.daemon.stopConfirmBody")}
            </div>
            <div
              style={{
                display: "flex",
                justifyContent: "flex-end",
                gap: 8,
                marginTop: 12,
              }}
            >
              <Btn onClick={() => setConfirmStop(false)}>
                {t("serviceTab.daemon.cancel")}
              </Btn>
              <Btn variant="destructive-primary" onClick={confirmStopNow}>
                {t("serviceTab.daemon.stopConfirmAction")}
              </Btn>
            </div>
          </div>
        </div>
      )}

      {confirmRotate && (
        <div
          className="voxix-sheet-backdrop"
          role="dialog"
          aria-modal="true"
          onClick={() => setConfirmRotate(false)}
        >
          <div
            className="voxix-card"
            style={{
              padding: 16,
              width: 380,
              boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
            }}
            onClick={(ev) => ev.stopPropagation()}
          >
            <div style={{ fontSize: "var(--text-base)", fontWeight: 600 }}>
              {t("serviceTab.token.rotateConfirmTitle")}
            </div>
            <div className="voxix-cap" style={{ marginTop: 4 }}>
              {t("serviceTab.token.rotateConfirmBody")}
            </div>
            <div
              style={{
                display: "flex",
                justifyContent: "flex-end",
                gap: 8,
                marginTop: 12,
              }}
            >
              <Btn onClick={() => setConfirmRotate(false)}>
                {t("serviceTab.daemon.cancel")}
              </Btn>
              <Btn variant="destructive-primary" onClick={confirmRotateNow}>
                {t("serviceTab.token.rotateConfirmAction")}
              </Btn>
            </div>
          </div>
        </div>
      )}
    </Screen>
  );
};
