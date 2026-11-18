// "VX" pixel mark — 16x16 grid, unit-square blocks, crisp edges.
// Geometry spec: docs/PLAN.md §A.1.
export const VxLogo = ({
  size = 16,
  className,
}: {
  size?: number;
  className?: string;
}) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 16 16"
    fill="currentColor"
    shapeRendering="crispEdges"
    className={className}
    aria-hidden="true"
  >
    <path d="M0 2h2v6h-2zM5 2h2v6h-2zM1 8h2v2h-2zM4 8h2v2h-2zM2 10h3v2h-3zM3 12h1v2h-1zM8 2h2v2h-2zM13 2h2v2h-2zM9 4h2v2h-2zM12 4h2v2h-2zM10 6h3v4h-3zM9 10h2v2h-2zM12 10h2v2h-2zM8 12h2v2h-2zM13 12h2v2h-2z" />
  </svg>
);
