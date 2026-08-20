import { bytes, cores } from "../format";

interface Props {
  used: number;
  total: number;
  /** How to render the raw numbers in the tooltip. */
  format: "cores" | "bytes" | "count";
  /** Shown instead of a bar when the source did not report. */
  unavailable?: boolean;
  unavailableReason?: string;
}

/**
 * A proportion at a glance.
 *
 * The bar is coloured by pressure rather than by a fixed accent: a node at 94%
 * memory and one at 12% should not look the same from across a room, which is
 * the whole reason to draw a bar instead of printing a number.
 */
export function UsageBar({ used, total, format, unavailable, unavailableReason }: Props) {
  if (unavailable || total <= 0) {
    return (
      <span className="usage usage--empty" title={unavailableReason ?? "no data"}>
        —
      </span>
    );
  }

  const fraction = Math.min(used / total, 1);
  const percent = fraction * 100;
  const tone = percent >= 90 ? "critical" : percent >= 75 ? "warn" : "ok";

  const render = (value: number) =>
    format === "cores" ? cores(value) : format === "bytes" ? bytes(value) : Math.round(value).toString();

  return (
    <span
      className="usage"
      title={`${render(used)} of ${render(total)} (${percent.toFixed(1)}%)`}
    >
      <span className="usage__track">
        <span className={`usage__fill usage__fill--${tone}`} style={{ width: `${percent}%` }} />
      </span>
      <span className="usage__value">{percent >= 10 ? percent.toFixed(0) : percent.toFixed(1)}%</span>
    </span>
  );
}
