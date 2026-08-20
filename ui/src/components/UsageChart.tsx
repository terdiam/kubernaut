import { useMemo, useState } from "react";
import { useStore } from "../store";
import { formatClock } from "../time";

/** Any regularly sampled series: a timestamp plus named numeric fields. */
export type ChartPoint = { at: number } & Record<string, number>;

interface Series {
  key: string;
  label: string;
  colour: string;
}

interface Props {
  samples: ChartPoint[];
  series: Series[];
  format: (value: number) => string;
  /** Shown when there is not enough history yet. */
  emptyHint: string;
}

const WIDTH = 720;
const HEIGHT = 220;
const PADDING = { top: 12, right: 52, bottom: 24, left: 8 };

/**
 * Time-series area chart, drawn as plain SVG.
 *
 * A charting library would add megabytes for one chart shape; the data is
 * already regularly sampled, so the maths is a linear scale and a path.
 */
export function UsageChart({ samples, series, format, emptyHint }: Props) {
  const [hover, setHover] = useState<number | null>(null);
  const zone = useStore((s) => s.preferences?.timezone ?? "system");

  const geometry = useMemo(() => {
    if (samples.length < 2) return null;

    const first = samples[0]!.at;
    const last = samples[samples.length - 1]!.at;
    const span = Math.max(last - first, 1);

    let max = 0;
    for (const sample of samples) {
      for (const entry of series) {
        max = Math.max(max, Number(sample[entry.key]) || 0);
      }
    }
    // A flat-zero series would divide by zero; give it a nominal ceiling so the
    // axis still renders.
    if (max <= 0) max = 1;
    max *= 1.15;

    const plotWidth = WIDTH - PADDING.left - PADDING.right;
    const plotHeight = HEIGHT - PADDING.top - PADDING.bottom;
    const x = (at: number) => PADDING.left + ((at - first) / span) * plotWidth;
    const y = (value: number) => PADDING.top + plotHeight - (value / max) * plotHeight;

    return { first, last, span, max, x, y, plotHeight, plotWidth };
  }, [samples, series]);

  if (!geometry) {
    return <p className="muted chart__empty">{emptyHint}</p>;
  }

  const { x, y, max, plotHeight } = geometry;
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((fraction) => max * fraction);
  const hovered = hover === null ? null : samples[hover];

  const areaPath = (key: string) => {
    const points = samples.map((s) => `${x(s.at)},${y(Number(s[key]) || 0)}`);
    const firstX = x(samples[0]!.at);
    const lastX = x(samples[samples.length - 1]!.at);
    const baseline = PADDING.top + plotHeight;
    return `M${firstX},${baseline} L${points.join(" L")} L${lastX},${baseline} Z`;
  };

  const linePath = (key: string) =>
    `M${samples.map((s) => `${x(s.at)},${y(Number(s[key]) || 0)}`).join(" L")}`;

  return (
    <div className="chart">
      <svg
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        preserveAspectRatio="none"
        className="chart__svg"
        onMouseLeave={() => setHover(null)}
        onMouseMove={(event) => {
          const bounds = event.currentTarget.getBoundingClientRect();
          const ratio = (event.clientX - bounds.left) / bounds.width;
          const target = geometry.first + ratio * geometry.span;
          let nearest = 0;
          let best = Infinity;
          samples.forEach((sample, index) => {
            const distance = Math.abs(sample.at - target);
            if (distance < best) {
              best = distance;
              nearest = index;
            }
          });
          setHover(nearest);
        }}
      >
        {ticks.map((tick) => (
          <g key={tick}>
            <line
              x1={PADDING.left}
              x2={WIDTH - PADDING.right}
              y1={y(tick)}
              y2={y(tick)}
              stroke="var(--border)"
              strokeWidth={1}
            />
            <text
              x={WIDTH - PADDING.right + 6}
              y={y(tick) + 4}
              className="chart__tick"
              fill="var(--muted)"
            >
              {format(tick)}
            </text>
          </g>
        ))}

        {series.map((entry) => (
          <g key={entry.key}>
            <path d={areaPath(entry.key)} fill={entry.colour} opacity={0.16} />
            <path d={linePath(entry.key)} fill="none" stroke={entry.colour} strokeWidth={1.6} />
          </g>
        ))}

        {hovered && (
          <line
            x1={x(hovered.at)}
            x2={x(hovered.at)}
            y1={PADDING.top}
            y2={PADDING.top + plotHeight}
            stroke="var(--accent)"
            strokeWidth={1}
            strokeDasharray="3 3"
          />
        )}
      </svg>

      <div className="chart__axis">
        <span>{formatClock(samples[0]!.at, zone)}</span>
        <span>{formatClock(samples[samples.length - 1]!.at, zone)}</span>
      </div>

      <div className="chart__legend">
        {series.map((entry) => (
          <span key={entry.key} className="chart__key">
            <span className="swatch" style={{ background: entry.colour }} />
            {entry.label}
            {hovered && <strong>{format(Number(hovered[entry.key]) || 0)}</strong>}
          </span>
        ))}
        {hovered && <span className="muted">{formatClock(hovered.at, zone)}</span>}
      </div>
    </div>
  );
}
