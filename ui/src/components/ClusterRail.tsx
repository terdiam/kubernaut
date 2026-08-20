import { useState } from "react";
import type { ClusterStatus, ContextEntry } from "../types";
import { useStore } from "../store";
import { AddCluster } from "./AddCluster";
import { ClusterMenu, ClusterSettings } from "./ClusterMenu";
import { RemoveCluster } from "./RemoveCluster";
import { Icon } from "./Icon";

const dotClass: Record<ClusterStatus["state"], string> = {
  connecting: "dot dot--pending",
  connected: "dot dot--ok",
  degraded: "dot dot--warn",
  unreachable: "dot dot--error",
  disconnected: "dot",
};

function initials(name: string): string {
  const parts = name.split(/[-_/:.\s]/).filter(Boolean);
  if (parts.length === 0) return name.slice(0, 2).toUpperCase();
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase();
  return (parts[0]![0]! + parts[parts.length - 1]![0]!).toUpperCase();
}

/**
 * The cluster rail.
 *
 * Only clusters the user added appear: the app does not inherit whatever is in
 * `~/.kube/config`, so nothing is one click away because a file happened to be
 * on disk. The first run is empty and says how to fix that.
 */
export function ClusterRail() {
  const contexts = useStore((s) => s.contexts);
  const clusters = useStore((s) => s.clusters);
  const active = useStore((s) => s.activeCluster);
  const connecting = useStore((s) => s.connecting);
  const connect = useStore((s) => s.connect);
  const preferences = useStore((s) => s.preferences);

  const [adding, setAdding] = useState(false);
  const [menu, setMenu] = useState<{ context: ContextEntry; x: number; y: number } | null>(null);
  const [settingsFor, setSettingsFor] = useState<ContextEntry | null>(null);
  const [removing, setRemoving] = useState<ContextEntry | null>(null);

  const profileOf = (name: string) => preferences?.clusterProfiles?.[name];

  return (
    <nav className="rail" aria-label="Clusters">
      {contexts.map((context, index) => {
        const summary = clusters[context.name];
        const state = summary?.status.state ?? "disconnected";
        const isActive = active === context.name;
        const busy = connecting === context.name;
        const profile = profileOf(context.name);
        const label = profile?.displayName || context.name;

        const title = [
          label,
          label === context.name ? "" : `context: ${context.name}`,
          context.server ?? "",
          context.missingExecPlugin
            ? `⚠ auth plugin "${context.execCommand}" not found on PATH`
            : "",
          index < 9 ? `⌘${index + 1}` : "",
          "right-click for options",
        ]
          .filter(Boolean)
          .join("\n");

        return (
          <button
            key={context.name}
            className={`rail__tile${isActive ? " rail__tile--active" : ""}`}
            style={
              profile?.colour
                ? ({ "--tile-accent": profile.colour } as React.CSSProperties)
                : undefined
            }
            title={title}
            disabled={busy}
            onClick={() => void connect(context.name)}
            onContextMenu={(event) => {
              event.preventDefault();
              setMenu({ context, x: event.clientX, y: event.clientY });
            }}
          >
            <span className="rail__initials">{initials(label)}</span>
            <span className={busy ? "dot dot--pending" : dotClass[state]} />
            {context.missingExecPlugin && <span className="rail__warn">!</span>}
          </button>
        );
      })}

      <button
        className="rail__tile rail__tile--add"
        title="Add a cluster"
        onClick={() => setAdding(true)}
      >
        <Icon name="plus" />
      </button>

      {menu && (
        <ClusterMenu
          context={menu.context}
          connected={Boolean(clusters[menu.context.name])}
          position={{ x: menu.x, y: menu.y }}
          onClose={() => setMenu(null)}
          onSettings={() => setSettingsFor(menu.context)}
          onRemove={() => setRemoving(menu.context)}
        />
      )}

      {settingsFor && (
        <ClusterSettings context={settingsFor} onClose={() => setSettingsFor(null)} />
      )}

      {removing && (
        <RemoveCluster context={removing} onClose={() => setRemoving(null)} />
      )}

      {adding && <AddCluster onClose={() => setAdding(false)} />}
    </nav>
  );
}
