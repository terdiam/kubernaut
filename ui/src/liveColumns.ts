/**
 * Columns whose values come from metrics rather than from the watched object.
 *
 * A node's manifest says what the machine has; only metrics say what it is
 * using. Rather than teach the generic table about metrics, a resource kind can
 * contribute extra columns here and the table merges them positionally.
 */

import { createElement, useEffect, useState, type ReactNode } from "react";
import { api } from "./api";

import { UsageBar } from "./components/UsageBar";
import type { ColumnSpec, NodeSummary, ResourceDescriptor, Row } from "./types";

/**
 * One cell. `text` is what sorting, filtering and the tooltip use; `node` is
 * what is drawn when a bar says it better than a number.
 */
export interface LiveCell {
  text: string;
  node?: ReactNode;
}

export interface LiveColumns {
  columns: ColumnSpec[];
  cells: (row: Row) => LiveCell[];
}

const EMPTY: LiveColumns = { columns: [], cells: () => [] };

/** How often live columns refresh. The sampler itself ticks every 15s. */
const POLL_MS = 5_000;

/// Percentage as text, for sorting and for the filter box.
function share(used: number, total: number): string {
  if (total <= 0) return "";
  return `${((used / total) * 100).toFixed(1)}%`;
}

export function useLiveColumns(
  cluster: string | null,
  resource: ResourceDescriptor | null,
): LiveColumns {
  const isNode = resource?.kind === "Node" && resource.group === "";
  const [summaries, setSummaries] = useState<Map<string, NodeSummary>>(new Map());

  useEffect(() => {
    if (!cluster || !isNode) {
      setSummaries(new Map());
      return;
    }
    let cancelled = false;

    const refresh = () =>
      void api
        .nodeSummaries(cluster)
        .then((rows) => {
          if (!cancelled) setSummaries(new Map(rows.map((row) => [row.name, row])));
        })
        .catch(() => {
          // Metrics being unavailable is not worth an error banner over the
          // table; the columns simply read "—".
        });

    refresh();
    const id = window.setInterval(refresh, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [cluster, isNode]);

  if (!isNode) return EMPTY;

  const columns: ColumnSpec[] = [
    {
      name: "CPU",
      kind: "string",
      priority: 0,
      description: "Cores in use, against what the scheduler may allocate",
    },
    {
      name: "Memory",
      kind: "string",
      priority: 0,
      description: "Memory in use, against allocatable",
    },
    {
      name: "Disk",
      kind: "string",
      priority: 0,
      description:
        "The filesystem the kubelet writes to — the one that triggers disk-pressure eviction",
    },
    {
      name: "Pods",
      kind: "string",
      priority: 0,
      description: "Pods scheduled here, against the node's pod limit",
    },
    { name: "OS", kind: "string", priority: 0, description: "Operating system and architecture" },
  ];

  const bar = (
    used: number,
    total: number,
    format: "cores" | "bytes" | "count",
    unavailable: boolean,
    reason: string,
  ): LiveCell => ({
    // Sorting and filtering work on the percentage, which is what the bar
    // shows; sorting on "1.4 / 16" would sort as text and be meaningless.
    text: unavailable ? "" : share(used, total),
    node: createElement(UsageBar, {
      used,
      total,
      format,
      unavailable,
      unavailableReason: reason,
    }),
  });

  const cells = (row: Row): LiveCell[] => {
    const summary = summaries.get(row.name);
    if (!summary) {
      return Array.from({ length: columns.length }, () => ({ text: "—" }));
    }

    const os = [summary.operatingSystem, summary.architecture].filter(Boolean).join("/");

    return [
      bar(
        summary.cpuUsage,
        summary.cpuAllocatable,
        "cores",
        !summary.usageAvailable,
        "metrics-server did not report this node",
      ),
      bar(
        summary.memoryUsage,
        summary.memoryAllocatable,
        "bytes",
        !summary.usageAvailable,
        "metrics-server did not report this node",
      ),
      bar(
        summary.diskUsed,
        summary.diskCapacity,
        "bytes",
        !summary.diskAvailable,
        "the kubelet summary endpoint is unavailable (needs nodes/proxy)",
      ),
      bar(summary.podsUsed, summary.podsAllocatable, "count", false, ""),
      { text: os || "—" },
    ];
  };

  return { columns, cells };
}
