import type { ContainerInfo } from "./types";

/**
 * Why the log fetch failed, in terms of the thing that actually went wrong.
 *
 * The kubelet's own wording is accurate but assumes you know that `kubectl
 * logs` reads a file on the node rather than anything the API server stores.
 * The most common failure — "unable to retrieve container logs for
 * containerd://…" — reads like a transient fault and is nothing of the kind:
 * the file is gone for good.
 */
export interface LogFailure {
  /** Machine-readable cause, for tests and styling. */
  code: string;
  title: string;
  /** What happened, and why the wording is misleading. */
  detail: string;
  /** What to do instead — or that there is nothing to do. */
  remedy: string;
  /** `true` when waiting or retrying could succeed later. */
  transient: boolean;
}

const RULES: {
  code: string;
  match: (message: string) => boolean;
  title: string;
  detail: string;
  remedy: string;
  transient: boolean;
}[] = [
  {
    code: "LogsGarbageCollected",
    match: (m) => m.includes("unable to retrieve container logs"),
    title: "The log file no longer exists on the node",
    detail:
      "Logs are not stored by Kubernetes. They are files under /var/log/pods on the node the " +
      "pod ran on, and the kubelet deletes them when it garbage-collects the dead container — " +
      "usually long before the pod object leaves etcd. The kubelet found its record of the " +
      "container and could not find the file.",
    remedy:
      "These logs cannot be recovered, and --previous points at the same missing file. What " +
      "survives is the container's exit status below. To keep logs beyond the node's retention, " +
      "ship them off-node; to stop dead pods outliving their logs, set ttlSecondsAfterFinished " +
      "on the Job.",
    transient: false,
  },
  {
    code: "NoPreviousInstance",
    match: (m) =>
      m.includes("previous terminated container") ||
      (m.includes("previous") && m.includes("not found")),
    title: "This container has no previous instance",
    detail:
      "Previous logs exist only after a restart. This container has not been restarted, so " +
      "there is no earlier instance to read.",
    remedy: "Turn off Previous to read the instance that is running now.",
    transient: false,
  },
  {
    code: "ContainerNotStarted",
    match: (m) =>
      m.includes("is waiting to start") ||
      m.includes("podinitializing") ||
      m.includes("containercreating"),
    title: "The container has not started yet",
    detail:
      "Nothing has been logged because the process has not run. The pod is still pulling an " +
      "image, waiting on an init container, or blocked on config the kubelet cannot resolve.",
    remedy: "The Diagnose tab says which of those it is.",
    transient: true,
  },
  {
    code: "Forbidden",
    match: (m) => m.includes("forbidden") || m.includes("cannot get resource \"pods/log\""),
    title: "Not allowed to read logs in this namespace",
    detail:
      "Reading logs needs `get` on the `pods/log` subresource, which is a separate grant from " +
      "listing pods — an account can see a pod and still be refused its logs.",
    remedy: "Ask for pods/log on this namespace, or open it with an account that has it.",
    transient: false,
  },
  {
    code: "NodeUnreachable",
    match: (m) =>
      m.includes("dial tcp") ||
      m.includes("no route to host") ||
      m.includes("connection refused") ||
      m.includes("i/o timeout") ||
      m.includes("context deadline exceeded"),
    title: "The node hosting this pod did not answer",
    detail:
      "The API server proxies log requests to the kubelet on the pod's node. That connection " +
      "failed, so this says nothing about the pod itself — the node is down, gone, or " +
      "unreachable from the control plane.",
    remedy: "Check the node's own status; the pod's logs are readable again once it is back.",
    transient: true,
  },
  {
    code: "PodGone",
    match: (m) => m.includes("not found") && m.includes("pod"),
    title: "The pod no longer exists",
    detail: "It was deleted, or its controller replaced it while this view was open.",
    remedy: "Tail the workload instead of one pod, so replacements are followed automatically.",
    transient: false,
  },
];

export function explainLogFailure(message: string): LogFailure | null {
  const lower = message.toLowerCase();
  const rule = RULES.find((candidate) => candidate.match(lower));
  if (!rule) return null;
  const { match: _match, ...failure } = rule;
  return failure;
}

/**
 * What is left of a container once its logs are gone.
 *
 * `status.containerStatuses` lives in etcd with the pod, so it outlives the log
 * file. For a job that failed months ago this is the only evidence there is.
 */
export function survivingFacts(container: ContainerInfo | undefined): string[] {
  if (!container) return [];
  const facts: string[] = [];

  if (container.state === "terminated" && container.exitCode !== null) {
    facts.push(
      `Exited ${container.exitCode}${container.reason ? ` (${container.reason})` : ""}`,
    );
  } else if (container.reason) {
    facts.push(`${container.state}: ${container.reason}`);
  }

  const ran = duration(container.startedAt, container.finishedAt);
  if (ran) facts.push(`Ran for ${ran}`);
  if (container.startedAt) facts.push(`Started ${container.startedAt}`);
  if (container.finishedAt) facts.push(`Finished ${container.finishedAt}`);
  if (container.restarts > 0) facts.push(`${container.restarts} restart(s)`);

  return facts;
}

/** How long the container ran, which narrows the cause more than the exit code alone. */
export function duration(from: string | null, to: string | null): string | null {
  if (!from || !to) return null;
  const start = Date.parse(from);
  const end = Date.parse(to);
  if (Number.isNaN(start) || Number.isNaN(end) || end < start) return null;
  // Kubernetes writes a zero `startedAt` for a container that never ran.
  // Subtracting from the epoch would claim it ran for decades.
  if (start <= 0) return null;

  const seconds = Math.round((end - start) / 1000);
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  const hours = Math.floor(seconds / 3600);
  return `${hours}h ${Math.floor((seconds % 3600) / 60)}m`;
}
