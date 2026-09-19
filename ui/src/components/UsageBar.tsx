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

interface PodUsageProps {
  used: number;
  request: number;
  limit: number;
  format: "cores" | "bytes";
  /** False until metrics-server has reported this pod. */
  available: boolean;
}

/**
 * One pod's CPU or memory.
 *
 * Unlike a node, a pod has no fixed capacity to fill: the number that matters is
 * how much it is using, and the bar (drawn only when a limit exists) says how
 * close that is to the point where it is throttled or killed. A pod with no
 * limit shows the number alone rather than a bar against an invented ceiling.
 */
export function PodUsageValue({ used, request, limit, format, available }: PodUsageProps) {
  if (!available) {
    return (
      <span className="usage usage--empty" title="metrics-server has not reported this pod yet">
        —
      </span>
    );
  }

  const render = format === "cores" ? cores : bytes;
  const declared = [
    request > 0 ? `requests ${render(request)}` : "no request",
    limit > 0 ? `limit ${render(limit)}` : "no limit",
  ].join(" · ");

  if (limit <= 0) {
    return (
      <span className="usage" title={declared}>
        <span className="usage__value usage__value--wide">{render(used)}</span>
      </span>
    );
  }

  const percent = Math.min(used / limit, 1) * 100;
  const tone = percent >= 90 ? "critical" : percent >= 75 ? "warn" : "ok";
  return (
    <span
      className="usage"
      title={`${render(used)} of ${render(limit)} limit (${percent.toFixed(0)}%) · ${declared}`}
    >
      <span className="usage__track">
        <span className={`usage__fill usage__fill--${tone}`} style={{ width: `${percent}%` }} />
      </span>
      <span className="usage__value usage__value--wide">{render(used)}</span>
    </span>
  );
}
