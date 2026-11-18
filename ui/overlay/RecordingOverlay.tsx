import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useRef, useState } from "react";
import { commands } from "@/bindings";
import "./RecordingOverlay.css";

type OverlayState = "recording" | "streaming" | "transcribing" | "processing" | "error";

const RecordingOverlay: React.FC = () => {
  const [visible, setVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
  const [errorMsg, setErrorMsg] = useState("Something went wrong");
  const smooth = useRef<number[]>(Array(16).fill(0));
  const waveRef = useRef<HTMLDivElement>(null);
  const recRef = useRef<HTMLDivElement>(null);
  const rafRef = useRef<number>(0);
  const recRafRef = useRef<number>(0);
  const firedRef = useRef(false);

  useEffect(() => {
    const setup = async () => {
      const unShow = await listen("show-overlay", (event) => {
        setState(event.payload as OverlayState);
        setVisible(true);
        firedRef.current = false;
      });
      const unHide = await listen("hide-overlay", () => setVisible(false));
      const unError = await listen<string>("overlay-error-message", (event) =>
        setErrorMsg(event.payload || "Something went wrong"),
      );
      const unLevel = await listen<number[]>("mic-level", (event) => {
        smooth.current = smooth.current.map((prev, i) => {
          const target = event.payload[i] || 0;
          return prev * 0.8 + target * 0.2;
        });
      });
      return () => {
        unShow();
        unHide();
        unError();
        unLevel();
      };
    };
    let cleanup: (() => void) | undefined;
    setup().then((fn) => (cleanup = fn));
    return () => cleanup?.();
  }, []);

  const recording = state === "recording" || state === "streaming";
  const busy = state === "transcribing" || state === "processing";
  const error = state === "error";

  const cancel = () => {
    if (!recording || firedRef.current) return;
    firedRef.current = true;
    commands
      .cancelOperation()
      .catch((e) => console.error("Failed to cancel operation:", e));
  };

  useEffect(() => {
    if (!busy || !waveRef.current) {
      cancelAnimationFrame(rafRef.current);
      return;
    }
    const rects = waveRef.current.querySelectorAll<SVGRectElement>("rect");
    const bases = [4, 8, 12, 8, 4];
    const start = performance.now();
    const tick = (now: number) => {
      const t = (now - start) / 800;
      rects.forEach((r, i) => {
        const phase = (i * 0.12) / 0.8;
        const v = (Math.sin((t - phase) * Math.PI * 2) + 1) / 2;
        const h = bases[i] * (0.35 + v * 0.65);
        r.setAttribute("height", String(h));
        r.setAttribute("y", String((12 - h) / 2));
      });
      rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafRef.current);
  }, [busy, visible]);

  useEffect(() => {
    if (!recording || !recRef.current) {
      cancelAnimationFrame(recRafRef.current);
      return;
    }
    const rects = recRef.current.querySelectorAll<SVGRectElement>("rect");
    const bases = [4, 6, 8, 6, 4];
    const tick = () => {
      const mean =
        smooth.current.reduce((a, b) => a + b, 0) / smooth.current.length;
      const level = Math.min(1, mean * 3);
      rects.forEach((r, i) => {
        const scale = 0.2 + level * 0.8;
        const h = Math.max(2, bases[i] * scale);
        r.setAttribute("height", String(h));
        r.setAttribute("y", String((12 - h) / 2));
      });
      recRafRef.current = requestAnimationFrame(tick);
    };
    recRafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(recRafRef.current);
  }, [recording, visible]);

  if (!visible) return <div className="voxix-overlay-root" />;

  const width = recording || busy ? 56 : error ? 300 : 16;
  const bases = [4, 6, 8, 6, 4];

  return (
    <div className="voxix-overlay-root">
      <div className="voxix-pill-wrap">
        <div
          className={`voxix-pill ${recording ? "rec" : ""} ${busy ? "busy" : ""} ${error ? "error" : ""}`}
          style={{ width }}
          title={recording ? "Click to cancel" : undefined}
          onClick={cancel}
        >
          {recording && (
            <div className="voxix-pill-wave" ref={recRef}>
              <svg
                width="22"
                height="12"
                viewBox="0 0 22 12"
                fill="none"
                xmlns="http://www.w3.org/2000/svg"
              >
                {[0, 1, 2, 3, 4].map((i) => (
                  <rect
                    key={i}
                    x={i * 5}
                    y={(12 - bases[i]) / 2}
                    width="2"
                    height={bases[i]}
                    rx="1"
                    fill="#fff"
                    fillOpacity="0.8"
                  />
                ))}
              </svg>
            </div>
          )}
          {busy && (
            <div className="voxix-pill-wave" ref={waveRef}>
              <svg
                width="22"
                height="12"
                viewBox="0 0 22 12"
                fill="none"
                xmlns="http://www.w3.org/2000/svg"
              >
                {[0, 1, 2, 3, 4].map((i) => (
                  <rect
                    key={i}
                    x={i * 5}
                    y={(12 - [4, 8, 12, 8, 4][i]) / 2}
                    width="2"
                    height={[4, 8, 12, 8, 4][i]}
                    rx="1"
                    fill="#fff"
                    fillOpacity="0.8"
                  />
                ))}
              </svg>
            </div>
          )}
          {error && (
            <div className="voxix-pill-error">
              <svg
                width="12"
                height="12"
                viewBox="0 0 24 24"
                fill="none"
                aria-hidden="true"
              >
                <path
                  d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"
                  stroke="#ff6b61"
                  strokeWidth="2"
                  strokeLinejoin="round"
                  strokeLinecap="round"
                />
                <path
                  d="M12 9v4"
                  stroke="#ff6b61"
                  strokeWidth="2"
                  strokeLinecap="round"
                />
                <path
                  d="M12 17h.01"
                  stroke="#ff6b61"
                  strokeWidth="2"
                  strokeLinecap="round"
                />
              </svg>
              <span className="voxix-pill-error-text">{errorMsg}</span>
            </div>
          )}
        </div>
        {/* eslint-disable-next-line i18next/no-literal-string */}
        <div className={recording ? "voxix-pill-hint" : "voxix-pill-hint off"}>
          Click to cancel
        </div>
      </div>
    </div>
  );
};

export default RecordingOverlay;
