/**
 * Object properties, per kind.
 *
 * The YAML tab already shows everything; this is the opposite — the handful of
 * fields someone actually looks for when they open an object, named the way
 * they would ask for them.
 */

import { bytes } from "./format";
import { getPath } from "./path";

export interface Property {
  label: string;
  value: string;
  /** Rendered dimmer; used for identifiers rather than state. */
  muted?: boolean;
  help?: string;
}

export interface PropertySection {
  title: string;
  properties: Property[];
}

type Obj = Record<string, unknown>;

function text(object: Obj, path: string): string | null {
  const value = getPath(object, path);
  if (value === undefined || value === null) return null;
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return null;
}

function list(object: Obj, path: string): unknown[] {
  const value = getPath(object, path);
  return Array.isArray(value) ? value : [];
}

function record(object: Obj, path: string): Record<string, string> {
  const value = getPath(object, path);
  return value && typeof value === "object" ? (value as Record<string, string>) : {};
}

function push(into: Property[], label: string, value: string | null, extra?: Partial<Property>) {
  if (value === null || value === "") return;
  into.push({ label, value, ...extra });
}

function metadataSection(object: Obj): PropertySection {
  const properties: Property[] = [];
  push(properties, "Name", text(object, "metadata.name"));
  push(properties, "Namespace", text(object, "metadata.namespace"));
  push(properties, "Created", text(object, "metadata.creationTimestamp"));
  push(properties, "UID", text(object, "metadata.uid"), { muted: true });
  push(properties, "Resource version", text(object, "metadata.resourceVersion"), { muted: true });

  const labels = record(object, "metadata.labels");
  const annotations = record(object, "metadata.annotations");
  push(properties, "Labels", `${Object.keys(labels).length}`);
  push(properties, "Annotations", `${Object.keys(annotations).length}`);

  const owners = list(object, "metadata.ownerReferences")
    .map((owner) => {
      const o = owner as Obj;
      return `${o.kind}/${o.name}`;
    })
    .join(", ");
  push(properties, "Controlled by", owners || null);

  const finalizers = list(object, "metadata.finalizers").join(", ");
  push(properties, "Finalizers", finalizers || null, {
    help: "A deletion cannot complete until every finalizer is removed.",
  });

  return { title: "Metadata", properties };
}

function podSections(object: Obj): PropertySection[] {
  const scheduling: Property[] = [];
  push(scheduling, "Node", text(object, "spec.nodeName"));
  push(scheduling, "Pod IP", text(object, "status.podIP"));
  push(scheduling, "Host IP", text(object, "status.hostIP"));
  push(scheduling, "Phase", text(object, "status.phase"));
  push(scheduling, "QoS class", text(object, "status.qosClass"), {
    help: "Guaranteed pods are evicted last; BestEffort first.",
  });
  push(scheduling, "Priority class", text(object, "spec.priorityClassName"));
  push(scheduling, "Service account", text(object, "spec.serviceAccountName"));
  push(scheduling, "Restart policy", text(object, "spec.restartPolicy"));
  push(scheduling, "Started", text(object, "status.startTime"));
  push(
    scheduling,
    "Host network",
    getPath(object, "spec.hostNetwork") === true ? "yes" : null,
    { help: "Shares the node's network namespace, bypassing NetworkPolicy." },
  );

  const containers: Property[] = [];
  const statuses = list(object, "status.containerStatuses") as Obj[];
  for (const container of list(object, "spec.containers") as Obj[]) {
    const status = statuses.find((s) => s.name === container.name);
    const bits = [String(container.image ?? "")];
    if (status) {
      bits.push(status.ready ? "ready" : "not ready");
      if (Number(status.restartCount ?? 0) > 0) bits.push(`${status.restartCount} restarts`);
    }
    push(containers, String(container.name ?? ""), bits.join(" · "));
  }
  for (const container of list(object, "spec.initContainers") as Obj[]) {
    push(containers, `${container.name} (init)`, String(container.image ?? ""));
  }

  const storage: Property[] = [];
  for (const volume of list(object, "spec.volumes") as Obj[]) {
    const kind = Object.keys(volume).find((key) => key !== "name") ?? "unknown";
    push(storage, String(volume.name ?? ""), kind);
  }

  return [
    { title: "Scheduling", properties: scheduling },
    { title: "Containers", properties: containers },
    ...(storage.length > 0 ? [{ title: "Volumes", properties: storage }] : []),
  ];
}

function workloadSections(object: Obj): PropertySection[] {
  const rollout: Property[] = [];
  const desired = getPath(object, "spec.replicas");
  push(rollout, "Replicas", desired === undefined ? null : String(desired));
  push(rollout, "Ready", text(object, "status.readyReplicas") ?? "0");
  push(rollout, "Updated", text(object, "status.updatedReplicas") ?? "0");
  push(rollout, "Available", text(object, "status.availableReplicas") ?? "0");
  push(rollout, "Unavailable", text(object, "status.unavailableReplicas"));
  push(rollout, "Strategy", text(object, "spec.strategy.type") ?? text(object, "spec.updateStrategy.type"));
  push(rollout, "Max surge", text(object, "spec.strategy.rollingUpdate.maxSurge"));
  push(rollout, "Max unavailable", text(object, "spec.strategy.rollingUpdate.maxUnavailable"));
  push(rollout, "Min ready", text(object, "spec.minReadySeconds"));
  push(rollout, "Revision history", text(object, "spec.revisionHistoryLimit"));
  push(rollout, "Generation", text(object, "metadata.generation"), { muted: true });
  push(rollout, "Observed generation", text(object, "status.observedGeneration"), {
    muted: true,
    help: "Lagging behind `generation` means the controller has not yet acted on the latest change.",
  });

  const selector = record(object, "spec.selector.matchLabels");
  push(
    rollout,
    "Selector",
    Object.entries(selector)
      .map(([k, v]) => `${k}=${v}`)
      .join(", ") || null,
  );

  const template: Property[] = [];
  for (const container of list(object, "spec.template.spec.containers") as Obj[]) {
    const resources = (container.resources ?? {}) as Obj;
    const requests = (resources.requests ?? {}) as Record<string, string>;
    const limits = (resources.limits ?? {}) as Record<string, string>;
    const bits = [String(container.image ?? "")];
    if (requests.cpu || requests.memory) {
      bits.push(`requests ${requests.cpu ?? "—"} / ${requests.memory ?? "—"}`);
    }
    if (limits.cpu || limits.memory) {
      bits.push(`limits ${limits.cpu ?? "—"} / ${limits.memory ?? "—"}`);
    }
    push(template, String(container.name ?? ""), bits.join(" · "));
  }
  push(template, "Service account", text(object, "spec.template.spec.serviceAccountName"));
  push(template, "Node selector", Object.entries(record(object, "spec.template.spec.nodeSelector"))
    .map(([k, v]) => `${k}=${v}`)
    .join(", ") || null);

  return [
    { title: "Rollout", properties: rollout },
    { title: "Pod template", properties: template },
  ];
}

function nodeSections(object: Obj): PropertySection[] {
  const system: Property[] = [];
  const info = record(object, "status.nodeInfo") as unknown as Obj;
  push(system, "Kubelet", String(info.kubeletVersion ?? ""));
  push(system, "Container runtime", String(info.containerRuntimeVersion ?? ""));
  push(system, "OS image", String(info.osImage ?? ""));
  push(system, "Kernel", String(info.kernelVersion ?? ""));
  push(system, "Architecture", String(info.architecture ?? ""));
  push(system, "Unschedulable", getPath(object, "spec.unschedulable") === true ? "yes" : null);
  push(system, "Pod CIDR", text(object, "spec.podCIDR"));

  const capacity = record(object, "status.capacity");
  const allocatable = record(object, "status.allocatable");
  const resources: Property[] = [];
  for (const key of ["cpu", "memory", "pods", "ephemeral-storage"]) {
    const cap = capacity[key];
    const alloc = allocatable[key];
    if (!cap && !alloc) continue;
    const format = (value?: string) => {
      if (!value) return "—";
      if (key === "memory" || key === "ephemeral-storage") {
        const parsed = parseQuantity(value);
        return parsed === null ? value : bytes(parsed);
      }
      return value;
    };
    push(resources, key, `${format(alloc)} allocatable of ${format(cap)}`);
  }

  const taints = list(object, "spec.taints")
    .map((taint) => {
      const t = taint as Obj;
      return `${t.key}${t.value ? `=${t.value}` : ""}:${t.effect}`;
    })
    .join(", ");
  push(system, "Taints", taints || null);

  return [
    { title: "System", properties: system },
    { title: "Capacity", properties: resources },
  ];
}

/** Minimal quantity parse for display only; the Rust side owns the real one. */
function parseQuantity(value: string): number | null {
  const match = /^([0-9.]+)([A-Za-z]*)$/.exec(value.trim());
  if (!match) return null;
  const number = Number(match[1]);
  const suffix = match[2] ?? "";
  const factors: Record<string, number> = {
    "": 1,
    Ki: 1024,
    Mi: 1024 ** 2,
    Gi: 1024 ** 3,
    Ti: 1024 ** 4,
    k: 1e3,
    M: 1e6,
    G: 1e9,
    T: 1e12,
  };
  const factor = factors[suffix];
  return factor === undefined ? null : number * factor;
}

function serviceSections(object: Obj): PropertySection[] {
  const properties: Property[] = [];
  push(properties, "Type", text(object, "spec.type"));
  push(properties, "Cluster IP", text(object, "spec.clusterIP"));
  push(properties, "Session affinity", text(object, "spec.sessionAffinity"));
  push(properties, "External traffic policy", text(object, "spec.externalTrafficPolicy"));
  const ports = list(object, "spec.ports")
    .map((port) => {
      const p = port as Obj;
      return `${p.port}${p.targetPort ? `→${p.targetPort}` : ""}/${p.protocol ?? "TCP"}`;
    })
    .join(", ");
  push(properties, "Ports", ports || null);
  const selector = Object.entries(record(object, "spec.selector"))
    .map(([k, v]) => `${k}=${v}`)
    .join(", ");
  push(properties, "Selector", selector || null);
  return [{ title: "Service", properties }];
}

/** Sections for an object, most specific first. */
export function propertiesFor(kind: string, object: Obj): PropertySection[] {
  const specific: PropertySection[] = (() => {
    switch (kind) {
      case "Pod":
        return podSections(object);
      case "Deployment":
      case "StatefulSet":
      case "DaemonSet":
      case "ReplicaSet":
        return workloadSections(object);
      case "Node":
        return nodeSections(object);
      case "Service":
        return serviceSections(object);
      default:
        return [];
    }
  })();

  return [...specific, metadataSection(object)].filter(
    (section) => section.properties.length > 0,
  );
}

export interface Condition {
  type: string;
  status: string;
  reason: string | null;
  message: string | null;
  lastTransition: string | null;
}

export function conditionsOf(object: Obj): Condition[] {
  return list(object, "status.conditions").map((entry) => {
    const c = entry as Obj;
    return {
      type: String(c.type ?? ""),
      status: String(c.status ?? ""),
      reason: (c.reason as string) ?? null,
      message: (c.message as string) ?? null,
      lastTransition:
        (c.lastTransitionTime as string) ?? (c.lastProbeTime as string) ?? null,
    };
  });
}
