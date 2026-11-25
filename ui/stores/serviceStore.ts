import { create } from "zustand";
import {
  commands,
  type ServiceDiscoveryInfo,
  type ServiceProbeReport,
  type ServiceRotateResult,
  type TailscaleInfo,
} from "@/bindings";

// The GUI's service token is loaded from disk via service_get_gui_token.
// It is never persisted in this store — it's always re-read from disk.
// rotatedToken holds the newly-generated primary token (external SDK clients)
// after a rotation, displayed once then cleared. This store is intentionally
// NOT wrapped in persist.
interface ServiceStore {
  discovery: ServiceDiscoveryInfo | null;
  probe: ServiceProbeReport | null;
  tailscale: TailscaleInfo | null;
  loading: boolean;
  starting: boolean;
  stopping: boolean;
  rotating: boolean;
  error: string | null;
  rotatedToken: ServiceRotateResult | null;
  serviceToken: string | null;
  connectionUrl: string | null;
  refresh: () => Promise<void>;
  start: () => Promise<{ ok: boolean; started: boolean }>;
  stop: () => Promise<boolean>;
  rotateToken: () => Promise<boolean>;
  dismissRotatedToken: () => void;
  clearError: () => void;
}

const errText = (e: unknown): string =>
  typeof e === "string" ? e : e instanceof Error ? e.message : String(e);

export const useServiceStore = create<ServiceStore>()((set, get) => ({
  discovery: null,
  probe: null,
  tailscale: null,
  loading: false,
  starting: false,
  stopping: false,
  rotating: false,
  error: null,
  rotatedToken: null,
  serviceToken: null,
  connectionUrl: null,

  refresh: async () => {
    set({ loading: true });
    try {
      const [discoverRes, probeRes, tokenRes, tailscaleRes] = await Promise.all([
        commands.serviceDiscover(),
        commands.serviceProbe(),
        commands.serviceGetGuiToken(),
        commands.serviceGetTailscaleInfo().catch(() => ({ ip: null, dns: null, available: false })),
      ]);
      const discovery =
        discoverRes.status === "ok" ? discoverRes.data : null;
      const serviceToken =
        tokenRes.status === "ok" ? tokenRes.data : null;
      const connectionUrl =
        discovery?.port != null && serviceToken
          ? `http://127.0.0.1:${discovery.port}?api_key=${serviceToken}`
          : null;
      set({
        discovery,
        probe: probeRes.status === "ok" ? probeRes.data : null,
        tailscale: tailscaleRes,
        error: discoverRes.status === "error" ? discoverRes.error : null,
        serviceToken,
        connectionUrl,
        loading: false,
      });
    } catch (e) {
      set({ error: errText(e), loading: false });
    }
  },

  start: async () => {
    set({ starting: true, error: null });
    try {
      const res = await commands.serviceStart();
      if (res.status === "error") {
        set({ starting: false, error: res.error });
        return { ok: false, started: false };
      }
      await get().refresh();
      set({ starting: false });
      return { ok: true, started: !res.data.already_running };
    } catch (e) {
      set({ starting: false, error: errText(e) });
      return { ok: false, started: false };
    }
  },

  stop: async () => {
    set({ stopping: true, error: null });
    try {
      const res = await commands.serviceStop();
      if (res.status === "error") {
        set({ stopping: false, error: res.error });
        return false;
      }
      await get().refresh();
      set({ stopping: false });
      return res.data.stopped;
    } catch (e) {
      set({ stopping: false, error: errText(e) });
      return false;
    }
  },

  rotateToken: async () => {
    set({ rotating: true, error: null });
    try {
      const res = await commands.serviceRotateToken();
      if (res.status === "error") {
        set({ rotating: false, error: res.error });
        return false;
      }
      // The rotated token is the new primary daemon token for external clients.
      // The GUI's own serviceToken (gui-client token) is unaffected by rotation.
      set({
        rotatedToken: res.data,
        rotating: false,
      });
      await get().refresh();
      return true;
    } catch (e) {
      set({ rotating: false, error: errText(e) });
      return false;
    }
  },

  dismissRotatedToken: () => set({ rotatedToken: null }),
  clearError: () => set({ error: null }),
}));
