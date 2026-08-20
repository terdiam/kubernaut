import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { bytes, cores, count, percent } from "../format";
import { useStore } from "../store";
import { Gauge } from "./Gauge";
import { IssuePanel } from "./IssuePanel";
import { Heatmap } from "./Heatmap";
import { TopologyView } from "./TopologyView";
import { UsageChart, type ChartPoint } from "./UsageChart";
import type { ClusterOverview, NodeScope, Sample } from "../types";

const SCOPES: { id: NodeScope; label: string }[] = [
  { id: "workers", label: "Worker Nodes" },
  { id: "controlPlane", label: "Control Plane" },
  { id: "all", label: "All Nodes" },
];

const WINDOWS: { id: string; label: string; ms: number }[] = [
  { id: "15m", label: "15m", ms: 15 * 60_000 },
  { id: "1h", label: "1h", ms: 60 * 60_000 },
];

type Metric = "cpu" | "memory" | "pods";
type Panel = "dashboard" | "heatmap" | "topology";

/** How often the UI asks for a fresh sample. The backend samples every 15s. */
const POLL_MS = 5_000;

export function Overview() {
  const cluster = useStore((s) => s.activeCluster);
  const scope = useStore((s) => s.overviewScope);
  const setScope = useStore((s) => s.setOverviewScope);

  const [overview, setOverview] = useState<ClusterOverview | null>(null);
  const [ready, setReady] = useState(false);
  const [samples, setSamples] = useState<Sample[]>([]);
  // `Sample` is a fixed struct from Rust; the chart takes an index signature.
  const points = useMemo<ChartPoint[]>(
    () => samples.map((sample) => ({ ...sample })),
    [samples],
  );
  const [windowId, setWindowId] = useState("1h");
  const [metric, setMetric] = useState<Metric>("cpu");
  const [panel, setPanel] = useState<Panel>("dashboard");
  const [error, setError] = useState<string | null>(null);

  const windowMs = WINDOWS.find((w) => w.id === windowId)?.ms ?? 3_600_000;

  useEffect(() => {
    if (!cluster || panel !== "dashboard") {
      if (!cluster) {
        setOverview(null);
        setSamples([]);
      }
      return;
    }
    let cancelled = false;

    const refresh = async () => {
      try {
        const [payload, history] = await Promise.all([
          api.clusterOverview(cluster, scope),
          api.overviewHistory(cluster, scope, windowMs),
        ]);
        if (cancelled) return;
        setOverview(payload.overview);
        setReady(payload.ready);
        setSamples(history);
        setError(null);
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    };

    void refresh();
    const id = window.setInterval(() => void refresh(), POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [cluster, scope, windowMs, panel]);

  const chart = useMemo(() => {
    switch (metric) {
      case "memory":
        return {
          format: bytes,
          series: [
            { key: "memoryUsage" as const, label: "Usage", colour: "var(--magenta)" },
            { key: "memoryRequests" as const, label: "Requests", colour: "var(--ok)" },
            { key: "memoryLimits" as const, label: "Limits", colour: "var(--pending)" },
          ],
        };
      case "pods":
        return {
          format: count,
          series: [{ key: "pods" as const, label: "Pods", colour: "var(--ok)" }],
        };
      default:
        return {
          format: cores,
          series: [
            { key: "cpuUsage" as const, label: "Usage", colour: "var(--magenta)" },
            { key: "cpuRequests" as const, label: "Requests", colour: "var(--ok)" },
            { key: "cpuLimits" as const, label: "Limits", colour: "var(--pending)" },
          ],
        };
    }
  }, [metric]);

  if (!cluster) {
    return (
      <div className="overview overview--empty">
        <p className="muted">Connect a cluster to see its overview.</p>
      </div>
    );
  }

  return (
    <div className="overview">
      <nav className="overview__panels">
        {(["dashboard", "heatmap", "topology"] as Panel[]).map((entry) => (
          <button
            key={entry}
            className={`tab${panel === entry ? " tab--active" : ""}`}
            onClick={() => setPanel(entry)}
          >
            {entry === "dashboard" ? "Dashboard" : entry === "heatmap" ? "Heatmap" : "Topology"}
          </button>
        ))}
      </nav>

      {panel === "heatmap" && <Heatmap />}
      {panel === "topology" && <TopologyView />}

      {panel === "dashboard" && (
        <>
      <header className="overview__toolbar">
        <select value={scope} onChange={(e) => setScope(e.target.value as NodeScope)}>
          {SCOPES.map((entry) => (
            <option key={entry.id} value={entry.id}>
              {entry.label}
            </option>
          ))}
        </select>

        <select value={windowId} onChange={(e) => setWindowId(e.target.value)}>
          {WINDOWS.map((entry) => (
            <option key={entry.id} value={entry.id}>
              {entry.label}
            </option>
          ))}
        </select>

        <div className="segmented">
          {(["cpu", "memory", "pods"] as Metric[]).map((entry) => (
            <button
              key={entry}
              className={`segmented__button${metric === entry ? " segmented__button--active" : ""}`}
              onClick={() => setMetric(entry)}
            >
              {entry === "cpu" ? "CPU" : entry === "memory" ? "Memory" : "Pods"}
            </button>
          ))}
        </div>

        {overview && (
          <span className="muted overview__nodes">
            {overview.nodes.ready}/{overview.nodes.total} nodes ready
            {overview.nodes.unschedulable > 0
              ? ` · ${overview.nodes.unschedulable} cordoned`
              : ""}
          </span>
        )}
      </header>

      {error && <p className="error overview__note">{error}</p>}

      {overview && !overview.metricsAvailable && (
        <p className="warning overview__note">
          Usage is unavailable: {overview.metricsError ?? "metrics.k8s.io did not respond"}.
          Requests, limits and capacity below come from the objects themselves and are accurate.
        </p>
      )}

      {!overview && (
        <p className="muted overview__note">
          {ready ? "Collecting the first sample…" : "Loading cluster state…"}
        </p>
      )}

      {overview && (
        <>
          <div className="overview__body">
            <div className="overview__chart">
              <UsageChart
                samples={points}
                series={chart.series}
                format={chart.format}
                emptyHint="Collecting history — the first points appear within a minute."
              />
            </div>

            <div className="overview__gauges">
              <Gauge title="CPU" gauge={overview.cpu} format={cores} />
              <Gauge title="Memory" gauge={overview.memory} format={bytes} />
              <Gauge title="Pods" gauge={overview.pods} format={count} />
            </div>
          </div>

          <div className="overview__summary muted">
            CPU {percent(overview.cpu.usage, overview.cpu.allocatable)} of allocatable ·
            memory {percent(overview.memory.usage, overview.memory.allocatable)} ·
            pods {percent(overview.pods.usage, overview.pods.allocatable)} of{" "}
            {count(overview.pods.allocatable)} slots
          </div>

          <IssuePanel issues={overview.issues} />
        </>
      )}
        </>
      )}
    </div>
  );
}
