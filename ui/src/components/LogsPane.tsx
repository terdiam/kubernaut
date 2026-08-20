import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { api, startLogs } from "../api";
import { useStore } from "../store";
import { localiseLogLine } from "../time";
import type { ContainerInfo, LogEvent, LogTarget } from "../types";

interface Props {
  cluster: string;
  target: LogTarget;
  /** Shown in the container picker; empty for workload targets. */
  pod: string | null;
  namespace: string;
}

interface Line {
  id: number;
  pod: string;
  container: string;
  text: string;
  /** Rendered differently: markers, not log output. */
  meta?: "dropped" | "ended" | "failed";
}

const LINE_HEIGHT = 18;
/** Lines kept in the DOM store. The Rust ring already bounds what arrives. */
const MAX_LINES = 20_000;

/** Stable colour per pod so interleaved output stays readable. */
function podColour(pod: string): string {
  let hash = 0;
  for (let i = 0; i < pod.length; i += 1) hash = (hash * 31 + pod.charCodeAt(i)) | 0;
  return `hsl(${Math.abs(hash) % 360} 70% 68%)`;
}

export function LogsPane({ cluster, target, pod, namespace }: Props) {
  const [lines, setLines] = useState<Line[]>([]);
  const [containers, setContainers] = useState<ContainerInfo[]>([]);
  const [container, setContainer] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [follow, setFollow] = useState(true);
  const [wrap, setWrap] = useState(false);
  const [timestamps, setTimestamps] = useState(false);
  const [previous, setPrevious] = useState(false);
  const [tailLines, setTailLines] = useState(500);
  const [error, setError] = useState<string | null>(null);

  const zone = useStore((s) => s.preferences?.timezone ?? "system");
  const scrollRef = useRef<HTMLDivElement>(null);
  const nextId = useRef(0);
  const multiPod = target.kind === "workload";

  useEffect(() => {
    if (!pod) {
      setContainers([]);
      return;
    }
    let cancelled = false;
    api
      .podContainers(cluster, namespace, pod)
      .then((list) => {
        if (!cancelled) setContainers(list);
      })
      .catch(() => {
        if (!cancelled) setContainers([]);
      });
    return () => {
      cancelled = true;
    };
  }, [cluster, namespace, pod]);

  const append = useCallback((events: LogEvent[]) => {
    setLines((current) => {
      const next = current.slice();
      for (const event of events) {
        const id = nextId.current++;
        switch (event.type) {
          case "line":
            next.push({ id, pod: event.pod, container: event.container, text: event.text });
            break;
          case "dropped":
            next.push({
              id,
              pod: "",
              container: "",
              text: `⚠ ${event.count} lines dropped — the pod is logging faster than this view can render`,
              meta: "dropped",
            });
            break;
          case "podEnded":
            next.push({
              id,
              pod: event.pod,
              container: "",
              text: `— ${event.pod}: ${event.reason}`,
              meta: "ended",
            });
            break;
          case "podFailed":
            next.push({
              id,
              pod: event.pod,
              container: "",
              text: `✕ ${event.pod}: ${event.message}`,
              meta: "failed",
            });
            break;
        }
      }
      return next.length > MAX_LINES ? next.slice(next.length - MAX_LINES) : next;
    });
  }, []);

  // Restart the stream whenever a source-affecting option changes.
  useEffect(() => {
    let stop: (() => void) | null = null;
    let cancelled = false;
    setLines([]);
    setError(null);

    startLogs(
      cluster,
      target,
      { container, tailLines, timestamps, previous, follow: true },
      append,
    )
      .then((session) => {
        if (cancelled) {
          session.stop();
          return;
        }
        stop = session.stop;
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      });

    return () => {
      cancelled = true;
      stop?.();
    };
    // `target` is an object literal from the caller; key on its parts.
  }, [
    cluster,
    append,
    container,
    tailLines,
    timestamps,
    previous,
    target.kind,
    target.name,
    target.namespace,
  ]);

  const visible = useMemo(() => {
    if (!filter.trim()) return lines;
    const needle = filter.toLowerCase();
    return lines.filter(
      (line) => line.text.toLowerCase().includes(needle) || line.pod.toLowerCase().includes(needle),
    );
  }, [lines, filter]);

  const virtualizer = useVirtualizer({
    count: visible.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => LINE_HEIGHT,
    overscan: 30,
    // Wrapped lines are taller than one row, so measure instead of assuming.
    measureElement: (element) => element.getBoundingClientRect().height,
  });

  useEffect(() => {
    if (follow && visible.length > 0) {
      virtualizer.scrollToIndex(visible.length - 1, { align: "end" });
    }
  }, [follow, visible.length, virtualizer]);

  const download = async () => {
    if (!pod) return;
    try {
      const text = await api.logSnapshot(cluster, namespace, pod, {
        container,
        tailLines: null,
        timestamps,
        previous,
      });
      const blob = new Blob([text], { type: "text/plain" });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `${pod}${container ? `-${container}` : ""}.log`;
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      setError(String(err));
    }
  };

  return (
    <div className="logs">
      <div className="logs__toolbar">
        {containers.length > 1 && (
          <select value={container ?? ""} onChange={(e) => setContainer(e.target.value || null)}>
            <option value="">All containers</option>
            {containers.map((c) => (
              <option key={c.name} value={c.name}>
                {c.name}
                {c.role !== "app" ? ` (${c.role})` : ""}
              </option>
            ))}
          </select>
        )}

        <select value={tailLines} onChange={(e) => setTailLines(Number(e.target.value))}>
          {[100, 500, 2000, 10000].map((n) => (
            <option key={n} value={n}>
              last {n}
            </option>
          ))}
        </select>

        <input
          className="logs__filter"
          value={filter}
          placeholder="Filter lines"
          onChange={(e) => setFilter(e.target.value)}
        />

        <label className="checkbox">
          <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
          Follow
        </label>
        <label className="checkbox">
          <input type="checkbox" checked={wrap} onChange={(e) => setWrap(e.target.checked)} />
          Wrap
        </label>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={timestamps}
            onChange={(e) => setTimestamps(e.target.checked)}
          />
          Timestamps
        </label>
        <label className="checkbox" title="Logs from the previous container instance — how to see why a crash-looping pod died">
          <input
            type="checkbox"
            checked={previous}
            onChange={(e) => setPrevious(e.target.checked)}
          />
          Previous
        </label>

        {pod && (
          <button className="button" onClick={() => void download()}>
            Download
          </button>
        )}
        <span className="muted logs__count">{visible.length} lines</span>
      </div>

      {error && <p className="error logs__error">{error}</p>}

      <div className={`logs__body${wrap ? " logs__body--wrap" : ""}`} ref={scrollRef}>
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((item) => {
            const line = visible[item.index];
            if (!line) return null;
            return (
              <div
                key={line.id}
                data-index={item.index}
                ref={virtualizer.measureElement}
                className={`logline${line.meta ? ` logline--${line.meta}` : ""}`}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  right: 0,
                  transform: `translateY(${item.start}px)`,
                }}
              >
                {multiPod && !line.meta && (
                  <span className="logline__pod" style={{ color: podColour(line.pod) }}>
                    {line.pod}
                  </span>
                )}
                <span className="logline__text">
                  {line.meta || !timestamps ? line.text : localiseLogLine(line.text, zone)}
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
