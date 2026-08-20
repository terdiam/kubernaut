import type { ResourceGauge } from "../types";

interface Props {
  title: string;
  gauge: ResourceGauge;
  format: (value: number) => string;
}

const SIZE = 132;
const STROKE = 13;
const RADIUS = (SIZE - STROKE) / 2;

/**
 * Donut showing usage, requests and limits against allocatable capacity.
 *
 * All three arcs share one denominator — allocatable — so their lengths are
 * directly comparable. Drawing each against its own maximum would make a small
 * usage next to a large request look identical to the reverse.
 */
export function Gauge({ title, gauge, format }: Props) {
  const denominator = gauge.allocatable > 0 ? gauge.allocatable : gauge.capacity;

  const rings = [
    { key: "limits", value: gauge.limits, colour: "var(--pending)", inset: 0 },
    { key: "requests", value: gauge.requests, colour: "var(--ok)", inset: STROKE + 3 },
    { key: "usage", value: gauge.usage, colour: "var(--magenta)", inset: (STROKE + 3) * 2 },
  ];

  return (
    <section className="gauge">
      <h3 className="gauge__title">{title}</h3>

      <svg
        className="gauge__chart"
        viewBox={`0 0 ${SIZE} ${SIZE}`}
        role="img"
        aria-label={`${title}: ${format(gauge.usage)} used of ${format(denominator)} allocatable`}
      >
        <g transform={`rotate(-90 ${SIZE / 2} ${SIZE / 2})`}>
          {rings.map((ring) => {
            const radius = RADIUS - ring.inset / 2;
            const circumference = 2 * Math.PI * radius;
            const length =
              denominator > 0 ? Math.min(ring.value / denominator, 1) * circumference : 0;
            return (
              <g key={ring.key}>
                <circle
                  cx={SIZE / 2}
                  cy={SIZE / 2}
                  r={radius}
                  fill="none"
                  stroke="var(--border)"
                  strokeWidth={STROKE - ring.inset / 6}
                />
                <circle
                  cx={SIZE / 2}
                  cy={SIZE / 2}
                  r={radius}
                  fill="none"
                  stroke={ring.colour}
                  strokeWidth={STROKE - ring.inset / 6}
                  strokeLinecap="round"
                  strokeDasharray={`${length} ${circumference}`}
                />
              </g>
            );
          })}
        </g>
      </svg>

      <dl className="gauge__legend">
        <div>
          <dt>
            <span className="swatch swatch--usage" />
            Usage
          </dt>
          <dd>{gauge.usageAvailable ? format(gauge.usage) : "unknown"}</dd>
        </div>
        <div>
          <dt>
            <span className="swatch swatch--requests" />
            Requests
          </dt>
          <dd>{format(gauge.requests)}</dd>
        </div>
        <div>
          <dt>
            <span className="swatch swatch--limits" />
            Limits
          </dt>
          <dd>{gauge.limits > 0 ? format(gauge.limits) : "none set"}</dd>
        </div>
        <div>
          <dt>
            <span className="swatch swatch--allocatable" />
            Allocatable
          </dt>
          <dd>{format(gauge.allocatable)}</dd>
        </div>
        <div>
          <dt>
            <span className="swatch" />
            Capacity
          </dt>
          <dd>{format(gauge.capacity)}</dd>
        </div>
      </dl>
    </section>
  );
}
