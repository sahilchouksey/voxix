import { create } from "zustand";
import {
  commands,
  type ServiceDiscoveryInfo,
  type ServiceProbeReport,
  type ServiceRotateResult,
} from "@/bindings";

// The service bearer token is never persisted: it lives in the Rust-side
// in-memory holder. The one-time post-rotate token lives in React state only
// (rotatedToken) and is cleared on dismiss. This store is intentionally NOT
// wrapped in persist.
interface ServiceStore {
  discovery: ServiceDiscoveryInfo | null;
  probe: ServiceProbeReport | null;
  loading: boolean;
  starting: boolean;
  stopping: boolean;
  rotating: boolean;
  error: string | null;
  rotatedToken: ServiceRotateResult | null;
  sessionToken: string | null;
  refresh: () => Promise<void>;
  start: () => Promise<{ ok: boolean; started: boolean }>;
  stop: () => Promise<boolean>;
  keepToken: (token: string) => Promise<void>;
  forgetToken: () => Promise<void>;
  rotateToken: () => Promise<boolean>;
  dismissRotatedToken: () => void;
  clearError: () => void;
}

const errText = (e: unknown): string =>
  typeof e === "string" ? e : e instanceof Error ? e.message : String(e);

export const useServiceStore = create<ServiceStore>()((set, get) => ({
  discovery: null,
  probe: null,
  loading: false,
  starting: false,
  stopping: false,
  rotating: false,
  error: null,
  rotatedToken: null,
  sessionToken: null,

  refresh: async () => {
    set({ loading: true });
    try {
      const [discoverRes, probeRes] = await Promise.all([
        commands.serviceDiscover(),
        commands.serviceProbe(),
      ]);
      set({
        discovery: discoverRes.status === "ok" ? discoverRes.data : null,
        probe: probeRes.status === "ok" ? probeRes.data : null,
        error: discoverRes.status === "error" ? discoverRes.error : null,
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

  keepToken: async (token: string) => {
    try {
      const res = await commands.serviceSetToken(token);
      if (res.status === "error") {
        set({ error: res.error });
        return;
      }
      await get().refresh();
      set({ sessionToken: token });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  forgetToken: async () => {
    try {
      const res = await commands.serviceSetToken(null);
      if (res.status === "error") {
        set({ error: res.error });
        return;
      }
      set({ sessionToken: null });
      await get().refresh();
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
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
      set({
        rotatedToken: res.data,
        rotating: false,
        sessionToken: res.data.token,
      });
      await get().refresh();
      return true;
    } catch (e) {
      set({ rotating: false, error: errText(e) });
      return false;
    }
  },

  dismissRotatedToken: () => set({ rotatedToken: null, sessionToken: null }),
  clearError: () => set({ error: null }),
}));
