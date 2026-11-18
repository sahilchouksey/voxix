import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useServiceStore } from "@/stores/serviceStore";
import { Badge, Btn, Group, Row, Screen, Toggle, useEscape } from "./ui";
import {
  CheckIcon,
  CopyIcon,
  ExclamationTriangle,
  LockShield,
  RefreshIcon,
  ServerIcon,
  XmarkCircle,
} from "./icons";

const EXAMPLES_URL =
  "https://github.com/sahilchouksey/voxix/tree/main/examples";

const MONO = { fontFamily: "var(--font-mono)" } as const;

const truncateId = (id: string) =>
  id.length > 16 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;

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
    loading,
    starting,
    stopping,
    rotating,
    error,
    rotatedToken,
    sessionToken,
    refresh,
    start,
    stop,
    keepToken,
    forgetToken,
    rotateToken,
    dismissRotatedToken,
    clearError,
  } = useServiceStore();

  const [confirmStop, setConfirmStop] = useState(false);
  const [confirmRotate, setConfirmRotate] = useState(false);
  const [showFingerprint, setShowFingerprint] = useState(false);
  const [tokenDraft, setTokenDraft] = useState("");
  const [embed, setEmbed] = useState(false);
  const [urlCopied, setUrlCopied] = useState(false);
  const [copied, setCopied] = useState(false);

  useEscape(confirmStop, () => setConfirmStop(false));
  useEscape(confirmRotate, () => setConfirmRotate(false));

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    setCopied(false);
  }, [rotatedToken]);

  useEffect(() => {
    if (!sessionToken) setEmbed(false);
  }, [sessionToken]);

  const embedToken = embed && !!sessionToken;

  const baseUrl =
    discovery?.port != null ? "http://127.0.0.1:" + discovery.port : null;
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
  const tokenPresent = probe?.token_present ?? false;

  const errorKey = probeErrorKey(healthError ?? statusError);
  const errorText = errorKey ? t(errorKey) : null;

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

      <Group
        title={t("serviceTab.daemon.groupTitle")}
        caption={
          discovery?.present
            ? undefined
            : t("serviceTab.daemon.discoveryMissing")
        }
      >
        <Row
          icon={<ServerIcon size={16} />}
          title={t("serviceTab.daemon.status")}
          caption={
            discovery?.present && stale
              ? t("serviceTab.daemon.discoveryStale")
              : undefined
          }
          control={
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span
                aria-hidden="true"
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  background: statusColor,
                  display: "inline-block",
                }}
              />
              <Badge>{statusLabel}</Badge>
              <button
                type="button"
                className="voxix-icon-btn"
                aria-label={t("serviceTab.daemon.refresh")}
                onClick={() => void refresh()}
                disabled={loading}
              >
                <RefreshIcon size={16} />
              </button>
            </div>
          }
        />
        {discovery?.present && discovery.port != null && (
          <Row
            title={t("serviceTab.daemon.port")}
            control={
              <span className="voxix-cap" style={MONO}>
                {discovery.port}
              </span>
            }
          />
        )}
        {discovery?.present && discovery.pid != null && (
          <Row
            title={t("serviceTab.daemon.processId")}
            control={
              <span className="voxix-cap" style={MONO}>
                {discovery.pid}
              </span>
            }
          />
        )}
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
        {health && (
          <>
            <Row
              title={t("serviceTab.daemon.engineVersion")}
              control={
                <span className="voxix-cap" style={MONO}>
                  {health.engine_version}
                </span>
              }
            />
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
            {uptimeSeconds != null && (
              <Row
                title={t("serviceTab.daemon.uptime")}
                control={
                  <span className="voxix-cap" style={MONO}>
                    {`${uptimeSeconds} s`}
                  </span>
                }
              />
            )}
          </>
        )}
        {discovery?.token_fingerprint && (
          <Row
            title={t("serviceTab.fingerprint.label")}
            control={
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <span className="voxix-cap" style={MONO}>
                  {showFingerprint
                    ? discovery.token_fingerprint
                    : "••••••••••••••••"}
                </span>
                <Btn onClick={() => setShowFingerprint((v) => !v)}>
                  {showFingerprint
                    ? t("serviceTab.fingerprint.hide")
                    : t("serviceTab.fingerprint.reveal")}
                </Btn>
              </div>
            }
          />
        )}
        {discovery?.present && baseUrl && (
          <Row
            title="Connect"
            caption="Any client on this machine can reach the daemon here. Every /v1 route needs the bearer token."
            control={
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <code style={{ ...MONO, fontSize: "var(--text-sm)" }}>
                  {baseUrl}
                </code>
                <button
                  type="button"
                  className="voxix-icon-btn"
                  aria-label="Copy service base URL"
                  onClick={async () => {
                    await writeText(baseUrl);
                    setUrlCopied(true);
                    setTimeout(() => setUrlCopied(false), 1500);
                  }}
                >
                  {urlCopied ? <CheckIcon size={14} /> : <CopyIcon size={14} />}
                </button>
              </div>
            }
          />
        )}
        {status?.active_model_id && (
          <Row
            title="Active model"
            control={
              <span className="voxix-cap" style={MONO}>
                {status.active_model_id} ({status.model_state})
              </span>
            }
          />
        )}
        {status?.backend && (
          <Row
            title="Backend"
            control={
              <span className="voxix-cap" style={MONO}>
                {status.backend}
              </span>
            }
          />
        )}
        {status?.capabilities && (
          <Row
            title="Capabilities"
            caption="Server-side VAD, auto gain, denoise and the one-stream limit reported by the service."
            control={
              <span className="voxix-cap" style={MONO}>
                {"streams:" +
                  status.capabilities.max_concurrent_streams +
                  " vad:" +
                  (status.capabilities.requires_server_vad
                    ? "server"
                    : "client") +
                  " gain:" +
                  (status.capabilities.prefers_auto_gain ? "auto" : "off") +
                  " denoise:" +
                  (status.capabilities.prefers_noise_reduction ? "on" : "off")}
              </span>
            }
          />
        )}
        <div
          className="voxix-row"
          style={{ justifyContent: "flex-end", gap: 8 }}
        >
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

      <Group
        title={t("serviceTab.token.groupTitle")}
        caption={t("serviceTab.token.memoryNote")}
        footer={discovery?.state_dir}
      >
        <Row
          icon={<LockShield size={16} />}
          title={t("serviceTab.token.inMemory")}
          control={
            tokenPresent ? (
              <Btn onClick={() => void forgetToken()}>
                {t("serviceTab.token.forget")}
              </Btn>
            ) : (
              <Badge>{t("serviceTab.token.pasteLabel")}</Badge>
            )
          }
        />
        {!tokenPresent && (
          <Row
            title={t("serviceTab.token.pasteLabel")}
            control={
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <input
                  type="password"
                  className="voxix-input"
                  style={{ width: 180, ...MONO }}
                  placeholder={t("serviceTab.token.pastePlaceholder")}
                  value={tokenDraft}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => setTokenDraft(e.target.value)}
                />
                <Btn
                  variant="primary"
                  disabled={tokenDraft.trim().length === 0}
                  onClick={() => {
                    const value = tokenDraft.trim();
                    setTokenDraft("");
                    void keepToken(value);
                  }}
                >
                  {t("serviceTab.token.keep")}
                </Btn>
              </div>
            }
          />
        )}
        <Row
          title={t("serviceTab.token.rotate")}
          control={
            <Btn
              onClick={() => setConfirmRotate(true)}
              disabled={rotating || !tokenPresent}
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

      <Group
        title={t("serviceTab.sessions.groupTitle")}
        caption={t("serviceTab.sessions.caption")}
      >
        <Row
          title={t("serviceTab.sessions.openSessions")}
          control={<Badge>{status?.active_sessions ?? 0}</Badge>}
        />
        <Row
          title={t("serviceTab.sessions.leaseHolder")}
          control={
            <span className="voxix-cap" style={MONO}>
              {status?.lease_held && status.active_session_id
                ? truncateId(status.active_session_id)
                : t("serviceTab.sessions.noLease")}
            </span>
          }
        />
        <Row
          title={t("serviceTab.sessions.worstRealtimeFactor")}
          control={
            <span className="voxix-cap" style={MONO}>
              {status?.max_realtime_factor != null
                ? status.max_realtime_factor.toFixed(2)
                : "—"}
            </span>
          }
        />
      </Group>

      <Group
        title="Use this service"
        caption="POST raw 16 kHz mono WAV bytes, then poll until the job completes. Authorization: Bearer <token>."
      >
        <Row
          title={t("serviceTab.examples.embedTitle")}
          caption={
            sessionToken ? undefined : t("serviceTab.examples.needsToken")
          }
          control={
            <Toggle
              on={embedToken}
              onChange={setEmbed}
              disabled={!sessionToken}
            />
          }
        />
        <Row
          title={t("serviceTab.examples.repoLinkLabel")}
          caption="Ready-to-run SDK and raw-HTTP (curl, Node, Python) scripts live in the repo's examples/ directory."
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
