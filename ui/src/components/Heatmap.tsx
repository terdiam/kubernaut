import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { bytes, cores } from "../format";
import { useStore } from "../store";
import type { NamespaceUsage } from "../types";

type Metric = "cpu" | "memory";

/**
 * Usage against declared requests, per namespace.
 *
 * The interesting number is the *ratio*, not the absolute: a namespace using
 * 4 cores against a 40-core request is wasting a scheduling reservation
 * everyone else pays for, and one using 4 against 1 will be throttled. Rows
 * with no request at all are marked rather than shown as an infinite ratio.
 */
export function Heatmap() {
  const cluster = useStore((s) => s.activeCluster);
  const [rows, setRows] = useState<NamespaceUsage[]>([]);
  const [metric, setMetric] = useState<Metric>("cpu");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!cluster) return;
    let cancelled = false;

    const refresh = async () => {
      try {
        const usage = await api.namespaceUsage(cluster);
        if (!cancelled) {
          setRows(usage);
          setError(null);
          setLoading(false);
        }
      } catch (err) {
        if (!cancelled) {
          setError(String(err));
          setLoading(false);
        }
      }
    };

    void refresh();
    const id = window.setInterval(() => void refresh(), 5000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [cluster]);

  const format = metric === "cpu" ? cores : bytes;

  const prepared = useMemo(() => {
    const usageOf = (row: NamespaceUsage) =>
      metric === "cpu" ? row.cpuUsage : row.memoryUsage;
    const requestOf = (row: NamespaceUsage) =>
      metric === "cpu" ? row.cpuRequests : row.memoryRequests;
    const limitOf = (row: NamespaceUsage) =>
      metric === "cpu" ? row.cpuLimits : row.memoryLimits;

    const maxUsage = Math.max(...rows.map(usageOf), 0.000001);

    return rows
      .map((row) => {
        const usage = usageOf(row);
        const request = requestOf(row);
        const limit = limitOf(row);
        const ratio = request > 0 ? usage / request : null;
        return { row, usage, request, limit, ratio, share: usage / maxUsage };
      })
      .sort((a, b) => b.usage - a.usage);
  }, [rows, metric]);

  /** Green when comfortably inside the request, amber when close, red over. */
  const ratioClass = (ratio: number | null) => {
    if (ratio === null) return "heat__ratio heat__ratio--unset";
    if (ratio > 1) return "heat__ratio heat__ratio--over";
    if (ratio > 0.85) return "heat__ratio heat__ratio--near";
    if (ratio < 0.2) return "heat__ratio heat__ratio--idle";
    return "heat__ratio";
  };

  if (loading) {
    return <p className="muted overview__note">Collecting pod metrics…</p>;
  }

  return (
    <div className="heat">
      <div className="heat__toolbar">
        <div className="segmented">
          {(["cpu", "memory"] as Metric[]).map((entry) => (
            <button
              key={entry}
              className={`segmented__button${metric === entry ? " segmented__button--active" : ""}`}
              onClick={() => setMetric(entry)}
            >
              {entry === "cpu" ? "CPU" : "Memory"}
            </button>
          ))}
        </div>
        <span className="muted">{prepared.length} namespaces</span>
      </div>

      {error && <p className="error overview__note">{error}</p>}

      {prepared.length === 0 && !error && (
        <p className="muted overview__note">
          No pod metrics yet. This needs metrics-server; the panel fills within a sample.
        </p>
      )}

      <table className="heat__table">
        <thead>
          <tr>
            <th>Namespace</th>
            <th className="heat__num">Pods</th>
            <th>Usage</th>
            <th className="heat__num">Requests</th>
            <th className="heat__num">Limits</th>
            <th className="heat__num">Usage / request</th>
          </tr>
        </thead>
        <tbody>
          {prepared.map(({ row, usage, request, limit, ratio, share }) => (
            <tr key={row.namespace}>
              <td className="heat__ns">{row.namespace}</td>
              <td className="heat__num">{row.pods}</td>
              <td className="heat__bar">
                <span className="heat__fill" style={{ width: `${Math.max(share * 100, 1)}%` }} />
                <span className="heat__value">{format(usage)}</span>
              </td>
              <td className="heat__num">{request > 0 ? format(request) : "—"}</td>
              <td className="heat__num">{limit > 0 ? format(limit) : "—"}</td>
              <td className="heat__num">
                <span className={ratioClass(ratio)}>
                  {ratio === null
                    ? row.hasUnsetRequests
                      ? "no request"
                      : "—"
                    : `${(ratio * 100).toFixed(0)}%`}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
