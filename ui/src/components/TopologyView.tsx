import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import type { Topology, TopologyNode } from "../types";

/** Left-to-right layers: how a request actually travels. */
const LAYERS = ["Ingress", "Service", "Workload", "Pod", "Node"];

const NODE_WIDTH = 168;
const NODE_HEIGHT = 44;
const LAYER_GAP = 96;
const ROW_GAP = 14;

interface Placed extends TopologyNode {
  x: number;
  y: number;
}

/**
 * Ingress → Service → Workload → Pod → Node.
 *
 * Layered by kind rather than force-directed: the layers *are* the meaning, and
 * a deterministic layout does not rearrange itself between refreshes, which
 * matters when someone is pointing at a node while explaining an incident.
 */
export function TopologyView() {
  const cluster = useStore((s) => s.activeCluster);
  const selectedNamespaces = useStore((s) => s.selectedNamespaces);
  const namespaces = useStore((s) => s.namespaces);

  const [graph, setGraph] = useState<Topology | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [hover, setHover] = useState<string | null>(null);

  // Cluster-wide would be a hairball; default to one namespace when the filter
  // is empty rather than rendering everything.
  const target = useMemo(() => {
    if (selectedNamespaces.length > 0) return selectedNamespaces;
    const fallback = namespaces.find((ns) => ns === "default") ?? namespaces[0];
    return fallback ? [fallback] : [];
  }, [selectedNamespaces, namespaces]);

  useEffect(() => {
    if (!cluster || target.length === 0) return;
    let cancelled = false;
    setLoading(true);
    api
      .topology(cluster, target)
      .then((result) => {
        if (!cancelled) {
          setGraph(result);
          setError(null);
        }
      })
      .catch((err) => !cancelled && setError(String(err)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [cluster, target.join(",")]);

  const layout = useMemo(() => {
    if (!graph) return null;

    const columns = LAYERS.map((layer) =>
      graph.nodes.filter((node) => node.kind === layer),
    );

    const placed: Placed[] = [];
    columns.forEach((column, index) => {
      column.forEach((node, row) => {
        placed.push({
          ...node,
          x: index * (NODE_WIDTH + LAYER_GAP),
          y: row * (NODE_HEIGHT + ROW_GAP),
        });
      });
    });

    const byId = new Map(placed.map((node) => [node.id, node]));
    const height =
      Math.max(...columns.map((c) => c.length), 1) * (NODE_HEIGHT + ROW_GAP) + ROW_GAP;
    const width = LAYERS.length * (NODE_WIDTH + LAYER_GAP);

    return { placed, byId, width, height, columns };
  }, [graph]);

  if (target.length === 0) {
    return <p className="muted overview__note">No namespaces to draw.</p>;
  }

  return (
    <div className="topo">
      <div className="topo__toolbar">
        <span className="muted">
          Namespaces: <strong>{target.join(", ")}</strong>
          {selectedNamespaces.length === 0 && " (pick namespaces above to change)"}
        </span>
        {loading && <span className="muted">loading…</span>}
        {graph?.truncated && (
          <span className="warning-text">
            Graph cut at 400 nodes — narrow the namespace filter for the full picture.
          </span>
        )}
      </div>

      {error && <p className="error overview__note">{error}</p>}

      {layout && layout.placed.length === 0 && (
        <p className="muted overview__note">Nothing to draw in this namespace.</p>
      )}

      {layout && layout.placed.length > 0 && (
        <div className="topo__canvas">
          <svg
            viewBox={`-12 -12 ${layout.width + 24} ${layout.height + 24}`}
            style={{ minWidth: layout.width, height: layout.height + 24 }}
          >
            {graph!.edges.map((edge, index) => {
              const from = layout.byId.get(edge.from);
              const to = layout.byId.get(edge.to);
              if (!from || !to) return null;
              const x1 = from.x + NODE_WIDTH;
              const y1 = from.y + NODE_HEIGHT / 2;
              const x2 = to.x;
              const y2 = to.y + NODE_HEIGHT / 2;
              const mid = (x1 + x2) / 2;
              const active = hover === edge.from || hover === edge.to;
              return (
                <path
                  key={`${edge.from}-${edge.to}-${index}`}
                  d={`M${x1},${y1} C${mid},${y1} ${mid},${y2} ${x2},${y2}`}
                  fill="none"
                  stroke={active ? "var(--accent)" : "var(--border)"}
                  strokeWidth={active ? 1.8 : 1}
                />
              );
            })}

            {layout.placed.map((node) => (
              <g
                key={node.id}
                transform={`translate(${node.x} ${node.y})`}
                className={`topo__node topo__node--${node.health}`}
                onMouseEnter={() => setHover(node.id)}
                onMouseLeave={() => setHover(null)}
              >
                <rect width={NODE_WIDTH} height={NODE_HEIGHT} rx={7} />
                <text x={10} y={18} className="topo__label">
                  {node.name.length > 22 ? `${node.name.slice(0, 21)}…` : node.name}
                </text>
                <text x={10} y={33} className="topo__sub">
                  {node.subKind ?? node.kind}
                  {node.detail ? ` · ${node.detail}` : ""}
                </text>
              </g>
            ))}

            {LAYERS.map((layer, index) => (
              <text
                key={layer}
                x={index * (NODE_WIDTH + LAYER_GAP)}
                y={-2}
                className="topo__layer"
              >
                {layer}
              </text>
            ))}
          </svg>
        </div>
      )}
    </div>
  );
}
