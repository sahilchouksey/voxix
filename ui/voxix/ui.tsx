import { type ReactNode, useEffect } from "react";

export const Group = ({
  title,
  caption,
  footer,
  children,
}: {
  title?: string;
  caption?: string;
  footer?: string;
  children: ReactNode;
}) => (
  <div style={{ width: "100%" }}>
    {title && <div className="voxix-section-title">{title}</div>}
    {caption && (
      <div className="voxix-cap" style={{ margin: "2px 0 10px" }}>
        {caption}
      </div>
    )}
    <div className="voxix-card">{children}</div>
    {footer && (
      <div className="voxix-cap" style={{ margin: "10px 0 0" }}>
        {footer}
      </div>
    )}
  </div>
);

export const Row = ({
  icon,
  title,
  caption,
  control,
}: {
  icon?: ReactNode;
  title: ReactNode;
  caption?: string;
  control?: ReactNode;
}) => (
  <div className="voxix-row">
    {icon && <div className="voxix-icon-col">{icon}</div>}
    <div style={{ flex: 1, minWidth: 0 }}>
      <div
        style={{
          fontSize: "var(--text-base)",
          lineHeight: "var(--leading-base)",
        }}
      >
        {title}
      </div>
      {caption && (
        <div className="voxix-cap" style={{ marginTop: 2 }}>
          {caption}
        </div>
      )}
    </div>
    {control}
  </div>
);

export const Screen = ({
  title,
  caption,
  center,
  children,
}: {
  title?: string;
  caption?: string;
  center?: boolean;
  children: ReactNode;
}) => (
  <div className="voxix-screen">
    {title && (
      <header
        className="voxix-screen-head"
        data-center={center ? "true" : undefined}
      >
        <h1 className="voxix-screen-title">{title}</h1>
        {caption && <p className="voxix-screen-caption">{caption}</p>}
      </header>
    )}
    {children}
  </div>
);

export const useEscape = (active: boolean, onEscape: () => void) => {
  useEffect(() => {
    if (!active) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") onEscape();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [active, onEscape]);
};

export const Toggle = ({
  on,
  onChange,
  disabled,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) => (
  <button
    type="button"
    className="voxix-switch"
    data-on={on}
    disabled={disabled}
    onClick={() => onChange(!on)}
    aria-pressed={on}
  />
);

export const Segmented = <T extends string>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
}) => (
  <div className="voxix-segmented">
    {options.map((o) => (
      <button
        key={o.value}
        type="button"
        data-active={o.value === value}
        aria-pressed={o.value === value}
        onClick={() => onChange(o.value)}
      >
        {o.label}
      </button>
    ))}
  </div>
);

export const Dots = ({ value, max = 5 }: { value: number; max?: number }) => (
  <span style={{ display: "inline-flex", alignItems: "center", gap: 3 }}>
    {Array.from({ length: max }, (_, i) => (
      <span
        key={i}
        style={{
          display: "inline-block",
          width: 7,
          height: 7,
          borderRadius: "50%",
          background: i < value ? "var(--accent)" : "rgba(255,255,255,0.2)",
        }}
      />
    ))}
  </span>
);

export const Badge = ({ children }: { children: ReactNode }) => (
  <span className="voxix-badge">{children}</span>
);

export const Btn = ({
  variant,
  onClick,
  disabled,
  children,
}: {
  variant?: "primary" | "destructive" | "destructive-primary";
  onClick?: () => void;
  disabled?: boolean;
  children: ReactNode;
}) => (
  <button
    type="button"
    className={`voxix-btn ${variant === "primary" ? "primary" : ""} ${
      variant === "destructive" ? "destructive" : ""
    } ${variant === "destructive-primary" ? "destructive primary" : ""}`}
    onClick={onClick}
    disabled={disabled}
  >
    {children}
  </button>
);

export const Slider = ({
  value,
  min,
  max,
  step,
  onChange,
  disabled,
}: {
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  disabled?: boolean;
}) => (
  <input
    type="range"
    className="voxix-slider"
    value={value}
    min={min}
    max={max}
    step={step}
    disabled={disabled}
    onChange={(e) => onChange(Number(e.target.value))}
  />
);
