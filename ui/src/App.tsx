import { useEffect } from "react";
import { ClusterRail } from "./components/ClusterRail";
import { ResourceSidebar } from "./components/ResourceSidebar";
import { ResourceTable } from "./components/ResourceTable";
import { Overview } from "./components/Overview";
import { HelmReleases } from "./components/HelmReleases";
import { HelmRepositories } from "./components/HelmRepositories";
import { GitOps } from "./components/GitOps";
import { SecurityCenter } from "./components/SecurityCenter";
import { Settings } from "./components/Settings";
import { UpdateBanner } from "./components/UpdateBanner";
import { DetailDrawer } from "./components/DetailDrawer";
import { NamespacePicker } from "./components/NamespacePicker";
import { CommandPalette } from "./components/CommandPalette";
import { ForwardsPanel } from "./components/ForwardsPanel";
import { onClusterStatus } from "./api";
import { useStore } from "./store";

function statusLabel(state: string, detail?: string) {
  switch (state) {
    case "connected":
      return `connected${detail ? ` · ${detail}` : ""}`;
    case "degraded":
      return "degraded";
    case "unreachable":
      return "unreachable";
    case "connecting":
      return "connecting…";
    default:
      return "disconnected";
  }
}

export default function App() {
  const loadContexts = useStore((s) => s.loadContexts);
  const loadPreferences = useStore((s) => s.loadPreferences);
  const applyStatus = useStore((s) => s.applyStatus);
  const contexts = useStore((s) => s.contexts);
  const connect = useStore((s) => s.connect);
  const activeCluster = useStore((s) => s.activeCluster);
  const clusters = useStore((s) => s.clusters);
  const resource = useStore((s) => s.resource);
  const view = useStore((s) => s.view);
  const rows = useStore((s) => s.rows);
  const filter = useStore((s) => s.filter);
  const setFilter = useStore((s) => s.setFilter);
  const watchState = useStore((s) => s.watchState);
  const error = useStore((s) => s.error);
  const dismissError = useStore((s) => s.dismissError);
  const forwardsOpen = useStore((s) => s.forwardsOpen);
  const toggleForwards = useStore((s) => s.toggleForwards);
  const paletteOpen = useStore((s) => s.paletteOpen);
  const setPaletteOpen = useStore((s) => s.setPaletteOpen);

  useEffect(() => {
    void loadContexts();
    void loadPreferences();
    const unlisten = onClusterStatus(({ cluster, status }) => applyStatus(cluster, status));
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [loadContexts, loadPreferences, applyStatus]);

  // Hotbar shortcuts (⌘/Ctrl + 1..9), command palette (⌘K), forwards (⌘P).
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      if (event.key === "k") {
        event.preventDefault();
        setPaletteOpen(true);
        return;
      }
      if (event.key === "p") {
        event.preventDefault();
        toggleForwards();
        return;
      }
      const index = Number.parseInt(event.key, 10) - 1;
      const target = contexts[index];
      if (Number.isNaN(index) || !target) return;
      event.preventDefault();
      void connect(target.name);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [contexts, connect, setPaletteOpen, toggleForwards]);

  const summary = activeCluster ? clusters[activeCluster] : undefined;
  const version = summary?.status.state === "connected" ? summary.status.version : undefined;

  return (
    <div className="app">
      <ClusterRail />

      <div className="app__main">
        <header className="topbar">
          <div className="topbar__cluster">
            <strong>{activeCluster ?? "No cluster"}</strong>
            {summary && (
              <span className={`pill pill--${summary.status.state}`}>
                {statusLabel(summary.status.state, version)}
              </span>
            )}
          </div>

          <NamespacePicker />

          <input
            className="topbar__filter"
            value={filter}
            placeholder={resource ? `Filter ${resource.plural}` : "Filter"}
            onChange={(e) => setFilter(e.target.value)}
            disabled={!resource}
          />

          <button
            className="button button--ghost"
            onClick={toggleForwards}
            title="Port forwards (⌘P)"
          >
            Forwards
          </button>

          <span className="muted topbar__count">
            {view === "resources" && resource ? `${rows.size} ${resource.plural}` : ""}
            {view === "resources" && watchState.state === "initializing" && resource
              ? " · loading…"
              : ""}
            {view === "resources" && watchState.state === "error" ? " · watch error" : ""}
          </span>
        </header>

        <UpdateBanner />

        {error && (
          <div className="banner">
            <span>{error}</span>
            <button className="icon-button" onClick={dismissError}>
              ✕
            </button>
          </div>
        )}

        {view === "resources" && watchState.state === "error" && (
          <div className="banner banner--warn">
            <span>{watchState.message}</span>
          </div>
        )}

        {contexts.length === 0 && (
          <div className="empty">
            <h2>No clusters yet</h2>
            <p className="muted">
              This app does not read <code>~/.kube/config</code> on its own, so nothing is
              reachable until you add it. Use the <strong>+</strong> on the left to import a
              context from your kubeconfig, choose a file, or paste one.
            </p>
          </div>
        )}

        <div className="app__body">
          <ResourceSidebar />
          <div className="app__center">
            {view === "overview" && <Overview />}
            {view === "resources" && <ResourceTable />}
            {view === "helmReleases" && <HelmReleases />}
            {view === "helmRepos" && <HelmRepositories />}
            {view === "gitops" && <GitOps />}
            {view === "security" && <SecurityCenter />}
            {view === "settings" && <Settings />}
            {forwardsOpen && <ForwardsPanel onClose={toggleForwards} />}
          </div>
          <DetailDrawer />
        </div>
      </div>

      {paletteOpen && <CommandPalette onClose={() => setPaletteOpen(false)} />}
    </div>
  );
}
