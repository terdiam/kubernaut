import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { bytes, cores } from "../format";
import { UsageChart, type ChartPoint } from "./UsageChart";
import type { MetricTarget, ObjectMetrics } from "../types";

interface Props {
  cluster: string;
  target: MetricTarget;
}

const WINDOWS = [
  { id: "15m", label: "15m", ms: 15 * 60_000 },
  { id: "1h", label: "1h", ms: 60 * 60_000 },
  { id: "6h", label: "6h", ms: 6 * 60 * 60_000 },
  { id: "24h", label: "24h", ms: 24 * 60 * 60_000 },
];

/** Network figures are rates, so the unit needs saying. */
function perSecond(value: number): string {
  return `${bytes(value)}/s`;
}

const SOURCE_LABEL: Record<ObjectMetrics["source"], string> = {
  prometheus: "Prometheus",
  metricsServer: "metrics-server (this session only)",
  none: "no source",
};

/** Usage charts for a single object, with its requests and limits for scale. */
export function ObjectMetricsPane({ cluster, target }: Props) {
  const [metrics, setMetrics] = useState<ObjectMetrics | null>(null);
  const [windowId, setWindowId] = useState("1h");
  const [error, setError] = useState<string | null>(null);

  const windowMs = WINDOWS.find((w) => w.id === windowId)?.ms ?? 3_600_000;
  const key = JSON.stringify(target);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const result = await api.objectMetrics(cluster, target, windowMs);
        if (!cancelled) {
          setMetrics(result);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    };
    void refresh();
    const id = window.setInterval(() => void refresh(), 10_000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [cluster, key, windowMs]);

  // Requests and limits become flat reference series so usage crossing a limit
  // is visible without reading numbers.
  const samples = useMemo<ChartPoint[]>(() => {
    if (!metrics) return [];
    return metrics.points.map((point) => ({
      at: point.at,
      cpuUsage: point.cpu,
      cpuRequests: metrics.cpuRequests,
      cpuLimits: metrics.cpuLimits,
      memoryUsage: point.memory,
      memoryRequests: metrics.memoryRequests,
      memoryLimits: metrics.memoryLimits,
    }));
  }, [metrics]);

  const ioSamples = useMemo<ChartPoint[]>(() => {
    if (!metrics) return [];
    return metrics.ioPoints.map((point) => ({
      at: point.at,
      networkRx: point.networkRx,
      networkTx: point.networkTx,
      fsUsed: point.fsUsed,
      volumeUsed: point.volumeUsed,
    }));
  }, [metrics]);

  return (
    <div className="objmetrics">
      <div className="objmetrics__toolbar">
        <select value={windowId} onChange={(e) => setWindowId(e.target.value)}>
          {WINDOWS.map((entry) => (
            <option key={entry.id} value={entry.id}>
              {entry.label}
            </option>
          ))}
        </select>
        {metrics && (
          <span className="muted">
            source: {SOURCE_LABEL[metrics.source]}
            {metrics.podCount > 1 ? ` · ${metrics.podCount} pods` : ""}
          </span>
        )}
        {error && <span className="error">{error}</span>}
      </div>

      {metrics?.note && <p className="muted overview__note">{metrics.note}</p>}

      {metrics && metrics.source !== "prometheus" && windowId !== "15m" && (
        <p className="muted overview__note">
          Without Prometheus, history only covers how long this app has been running.
        </p>
      )}

      <div className="objmetrics__charts">
        <section>
          <h4>CPU</h4>
          <UsageChart
            samples={samples}
            format={cores}
            emptyHint="No CPU samples yet."
            series={[
              { key: "cpuUsage", label: "Usage", colour: "var(--magenta)" },
              { key: "cpuRequests", label: "Requests", colour: "var(--ok)" },
              { key: "cpuLimits", label: "Limits", colour: "var(--pending)" },
            ]}
          />
        </section>

        <section>
          <h4>Memory</h4>
          <UsageChart
            samples={samples}
            format={bytes}
            emptyHint="No memory samples yet."
            series={[
              { key: "memoryUsage", label: "Usage", colour: "var(--magenta)" },
              { key: "memoryRequests", label: "Requests", colour: "var(--ok)" },
              { key: "memoryLimits", label: "Limits", colour: "var(--pending)" },
            ]}
          />
        </section>

        <section>
          <h4>Network</h4>
          {metrics?.ioNote && ioSamples.length === 0 ? (
            <p className="muted">{metrics.ioNote}</p>
          ) : (
            <UsageChart
              samples={ioSamples}
              format={perSecond}
              emptyHint="Collecting network counters — rates need two samples."
              series={[
                { key: "networkRx", label: "Received", colour: "var(--ok)" },
                { key: "networkTx", label: "Transmitted", colour: "var(--pending)" },
              ]}
            />
          )}
        </section>

        <section>
          <h4>Filesystem</h4>
          {metrics?.ioNote && ioSamples.length === 0 ? (
            <p className="muted">{metrics.ioNote}</p>
          ) : (
            <UsageChart
              samples={ioSamples}
              format={bytes}
              emptyHint="No filesystem samples yet."
              series={[
                { key: "fsUsed", label: "Writable layer + logs", colour: "var(--magenta)" },
                { key: "volumeUsed", label: "Volumes", colour: "var(--accent)" },
              ]}
            />
          )}
        </section>
      </div>
    </div>
  );
}
