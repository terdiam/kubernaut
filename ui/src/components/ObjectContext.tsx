import { useEffect, useState } from "react";
import { api } from "../api";
import { bytes, cores } from "../format";
import { conditionsOf, propertiesFor, type PropertySection } from "../properties";
import { useStore } from "../store";
import { toneForValue } from "../statusTone";
import { formatDateTime } from "../time";
import type { EventRow, NodeSummary, Related, RelatedRef } from "../types";

interface Props {
  cluster: string;
  resource: string;
  kind: string;
  namespace: string | null;
  name: string;
  /** Live object JSON, already fetched by the drawer. */
  object: Record<string, unknown> | null;
  /** Changes when the object is reloaded, to refetch context. */
  revision: number;
}

/** Properties, conditions, related objects and recent events for one object. */
export function ObjectContext(props: Props) {
  const { cluster, resource, kind, namespace, name, object, revision } = props;
  const [related, setRelated] = useState<Related | null>(null);
  const [events, setEvents] = useState<EventRow[]>([]);
  const [node, setNode] = useState<NodeSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  // A node's usage is not in its manifest; it comes from metrics-server and the
  // pod store, so it is fetched alongside the object rather than derived.
  useEffect(() => {
    if (kind !== "Node") {
      setNode(null);
      return;
    }
    let cancelled = false;

    const refresh = () =>
      void api
        .nodeSummaries(cluster)
        .then((rows) => {
          if (!cancelled) setNode(rows.find((row) => row.name === name) ?? null);
        })
        .catch(() => {});

    refresh();
    const id = window.setInterval(refresh, 5000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [cluster, kind, name]);

  useEffect(() => {
    let cancelled = false;
    setError(null);

    void api
      .relatedResources(cluster, resource, namespace, name)
      .then(async (result) => {
        if (cancelled) return;
        setRelated(result);

        // A workload's own events are sparse; what people want is what its
        // pods are saying. Fall back to the object's own events for kinds with
        // no pods.
        const podNames = result.pods.map((pod) => pod.name);
        const rows =
          podNames.length > 0 && namespace
            ? [
                ...(await api.objectEvents(cluster, namespace, name)),
                ...(await api.podEvents(cluster, namespace, podNames)),
              ]
            : await api.objectEvents(cluster, namespace, name);
        if (!cancelled) {
          rows.sort((a, b) => (a.lastSeen ?? "").localeCompare(b.lastSeen ?? ""));
          setEvents(rows.slice(-60).reverse());
        }
      })
      .catch((err) => !cancelled && setError(String(err)));

    return () => {
      cancelled = true;
    };
  }, [cluster, resource, namespace, name, revision]);

  const zone = useStore((s) => s.preferences?.timezone ?? "system");

  const sections: PropertySection[] = [
    ...(node ? [usageSection(node)] : []),
    ...(object ? propertiesFor(kind, object) : []),
  ];
  const conditions = object ? conditionsOf(object) : [];

  return (
    <div className="context">
      {error && <p className="error">{error}</p>}

      {sections.map((section) => (
        <section key={section.title} className="context__block">
          <h3>{section.title}</h3>
          <dl className="props">
            {section.properties.map((property) => (
              <div key={`${section.title}-${property.label}`} className="props__row">
                <dt title={property.help}>{property.label}</dt>
                <dd className={property.muted ? "muted" : undefined}>
                  {looksLikeTimestamp(property.value)
                    ? formatDateTime(property.value, zone)
                    : property.value}
                </dd>
              </div>
            ))}
          </dl>
        </section>
      ))}

      {conditions.length > 0 && (
        <section className="context__block">
          <h3>Conditions</h3>
          <table className="conditions">
            <tbody>
              {conditions.map((condition) => (
                <tr key={condition.type}>
                  <td>
                    <span className={`chip chip--${conditionTone(condition.type, condition.status)}`}>
                      {condition.status}
                    </span>
                  </td>
                  <td className="conditions__type">{condition.type}</td>
                  <td className="muted">{condition.reason ?? ""}</td>
                  <td className="conditions__message" title={condition.message ?? ""}>
                    {condition.message ?? ""}
                  </td>
                  <td className="muted conditions__when">
                    {condition.lastTransition
                      ? formatDateTime(condition.lastTransition, zone)
                      : ""}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {related && <RelatedBlocks related={related} />}

      <section className="context__block">
        <h3>Recent events</h3>
        {events.length === 0 ? (
          <p className="muted">
            No events. Kubernetes discards them after about an hour, so quiet is normal.
          </p>
        ) : (
          <ul className="events">
            {events.map((event, index) => (
              <li
                key={`${event.object}-${event.reason}-${index}`}
                className={`events__row events__row--${event.kind.toLowerCase()}`}
              >
                <span className="events__reason">{event.reason}</span>
                <span className="events__object muted">{event.object}</span>
                <span className="events__message" title={event.message}>
                  {event.message}
                </span>
                <span className="events__meta muted">
                  {event.count > 1 ? `×${event.count}` : ""}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

/** RFC3339 as Kubernetes writes it. */
const TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/;

function looksLikeTimestamp(value: string): boolean {
  return TIMESTAMP.test(value);
}

/**
 * Colour for a condition.
 *
 * `status: True` is not automatically good: `MemoryPressure=True` and
 * `Ready=True` mean opposite things. Conditions whose name describes a problem
 * invert.
 */
function conditionTone(type: string, status: string): string {
  const negative = /pressure|unavailable|failed|degraded|error|conflict|deprecated/i.test(type);
  if (status === "True") return negative ? "error" : "ok";
  if (status === "False") return negative ? "ok" : "error";
  return toneForValue(status) ?? "unknown";
}

/// Live usage against what the scheduler may hand out.
function usageSection(node: NodeSummary): PropertySection {
  const share = (used: number, total: number) =>
    total > 0 ? ` (${Math.round((used / total) * 100)}%)` : "";

  const properties = [
    {
      label: "CPU usage",
      value: node.usageAvailable
        ? `${cores(node.cpuUsage)} of ${cores(node.cpuAllocatable)} allocatable${share(
            node.cpuUsage,
            node.cpuAllocatable,
          )}`
        : "metrics-server did not report this node",
    },
    {
      label: "CPU requested",
      value: `${cores(node.cpuRequests)}${share(node.cpuRequests, node.cpuAllocatable)}`,
      help: "Reserved by pods whether or not they use it — this is what the scheduler goes by.",
    },
    {
      label: "RAM usage",
      value: node.usageAvailable
        ? `${bytes(node.memoryUsage)} of ${bytes(node.memoryAllocatable)} allocatable${share(
            node.memoryUsage,
            node.memoryAllocatable,
          )}`
        : "metrics-server did not report this node",
    },
    {
      label: "RAM requested",
      value: `${bytes(node.memoryRequests)}${share(node.memoryRequests, node.memoryAllocatable)}`,
    },
    {
      label: "Disk",
      value: node.diskAvailable
        ? `${bytes(node.diskUsed)} of ${bytes(node.diskCapacity)}${share(
            node.diskUsed,
            node.diskCapacity,
          )}`
        : "kubelet summary unavailable (needs nodes/proxy)",
      help: "The filesystem the kubelet writes to; filling it triggers disk-pressure eviction.",
    },
    {
      label: "Image disk",
      value:
        node.imageDiskCapacity > 0
          ? `${bytes(node.imageDiskUsed)} of ${bytes(node.imageDiskCapacity)}${share(
              node.imageDiskUsed,
              node.imageDiskCapacity,
            )}`
          : "",
      help: "Where the container runtime stores images.",
    },
    {
      label: "Pods",
      value: `${Math.round(node.podsUsed)} of ${Math.round(node.podsAllocatable)}${share(
        node.podsUsed,
        node.podsAllocatable,
      )}`,
    },
    {
      label: "OS",
      value: [node.operatingSystem, node.architecture].filter(Boolean).join("/"),
    },
    { label: "OS image", value: node.osImage ?? "" },
    { label: "Kernel", value: node.kernelVersion ?? "" },
    { label: "Container runtime", value: node.containerRuntime ?? "" },
    { label: "Kubelet", value: node.kubeletVersion ?? "" },
  ].filter((property) => property.value !== "");

  return { title: "Usage", properties };
}

const GROUPS: { key: keyof Related; title: string }[] = [
  { key: "controllers", title: "Controlled by" },
  { key: "pods", title: "Pods" },
  { key: "services", title: "Services" },
  { key: "ingresses", title: "Ingresses" },
  { key: "policies", title: "Policies" },
  { key: "config", title: "Config" },
  { key: "storage", title: "Storage" },
  { key: "nodes", title: "Nodes" },
];

function RelatedBlocks({ related }: { related: Related }) {
  const openObject = useStore((s) => s.openObject);

  const row = (entry: RelatedRef) => (
    <li key={`${entry.kind}-${entry.namespace}-${entry.name}`}>
      <button
        className={`related__item related__item--${entry.health}`}
        disabled={entry.resource === ""}
        onClick={() => void openObject(entry.resource, entry.namespace, entry.name)}
      >
        <span className="related__kind">{entry.kind}</span>
        <span className="related__name">{entry.name}</span>
        <span className="muted related__detail">{entry.detail ?? ""}</span>
      </button>
    </li>
  );

  return (
    <>
      {GROUPS.filter((group) => related[group.key].length > 0).map((group) => (
        <section key={group.key} className="context__block">
          <h3>
            {group.title}
            <span className="context__count">{related[group.key].length}</span>
          </h3>
          <ul className="related">{related[group.key].map(row)}</ul>
        </section>
      ))}
    </>
  );
}
