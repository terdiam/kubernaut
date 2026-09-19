/**
 * Columns whose values come from metrics rather than from the watched object.
 *
 * A node's manifest says what the machine has; only metrics say what it is
 * using. Rather than teach the generic table about metrics, a resource kind can
 * contribute extra columns here and the table merges them positionally.
 */

import { createElement, useEffect, useState, type ReactNode } from "react";
import { api } from "./api";

import { PodUsageValue, UsageBar } from "./components/UsageBar";
import type { ColumnSpec, NodeSummary, PodUsage, ResourceDescriptor, Row } from "./types";

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
  const isPod = resource?.kind === "Pod" && resource.group === "";
  const [summaries, setSummaries] = useState<Map<string, NodeSummary>>(new Map());
  const [pods, setPods] = useState<Map<string, PodUsage>>(new Map());

  useEffect(() => {
    if (!cluster || !isPod) {
      setPods(new Map());
      return;
    }
    let cancelled = false;

    const refresh = () =>
      void api
        .podUsages(cluster, null)
        .then((rows) => {
          if (!cancelled) {
            setPods(new Map(rows.map((row) => [`${row.namespace}/${row.name}`, row])));
          }
        })
        .catch(() => {});

    refresh();
    const id = window.setInterval(refresh, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [cluster, isPod]);

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

  if (isPod) return podColumns(pods);
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

  return { columns: POD_COLUMNS, cells };
}

/**
 * Fixed for the life of the module: the table re-derives its column widths whenever
 * the column list changes identity, so a fresh array per render would re-run that
 * every render.
 */
const POD_COLUMNS: ColumnSpec[] = [
  {
    name: "CPU",
    kind: "string",
    priority: 0,
    description: "Cores in use; the bar is against the pod's limit, when it has one",
  },
  {
    name: "Memory",
    kind: "string",
    priority: 0,
    description: "Memory in use; the bar is against the pod's limit, when it has one",
  },
];

/** A pod the sampler has not heard about yet, or the first fetch still in flight. */
function unreported(format: "cores" | "bytes"): LiveCell {
  return {
    text: "",
    node: createElement(PodUsageValue, {
      used: 0,
      request: 0,
      limit: 0,
      format,
      available: false,
    }),
  };
}

/**
 * CPU and memory beside each pod.
 *
 * Sorting compares the cell text, so it carries the raw figure (millicores, MiB)
 * rather than the formatted one — "9.5MiB" would sort after "100MiB" as text.
 */
function podColumns(pods: Map<string, PodUsage>): LiveColumns {
  const cells = (row: Row): LiveCell[] => {
    const usage = pods.get(`${row.namespace ?? ""}/${row.name}`);
    if (!usage) return [unreported("cores"), unreported("bytes")];

    return [
      {
        text: usage.usageAvailable ? (usage.cpuUsage * 1000).toFixed(0) : "",
        node: createElement(PodUsageValue, {
          used: usage.cpuUsage,
          request: usage.cpuRequests,
          limit: usage.cpuLimits,
          format: "cores",
          available: usage.usageAvailable,
        }),
      },
      {
        text: usage.usageAvailable ? (usage.memoryUsage / 1024 / 1024).toFixed(1) : "",
        node: createElement(PodUsageValue, {
          used: usage.memoryUsage,
          request: usage.memoryRequests,
          limit: usage.memoryLimits,
          format: "bytes",
          available: usage.usageAvailable,
        }),
      },
    ];
  };

  return { columns: POD_COLUMNS, cells };
}
