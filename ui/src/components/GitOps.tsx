import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { age } from "../age";
import { useStore } from "../store";
import type { GitOpsEntry, GitOpsSummary } from "../types";

const CONTROLLER_LABEL: Record<string, string> = {
  argocd: "Argo CD",
  flux: "Flux",
  fleet: "Fleet",
};

/**
 * What the cluster is syncing, and whether it worked.
 *
 * Deliberately not a per-controller screen: Argo CD, Flux and Fleet store the
 * same idea in three shapes, and someone debugging a deployment cares which
 * commit is applied, not which controller applied it.
 */
export function GitOps() {
  const cluster = useStore((s) => s.activeCluster);
  const selectedNamespaces = useStore((s) => s.selectedNamespaces);
  const openObject = useStore((s) => s.openObject);

  const [summary, setSummary] = useState<GitOpsSummary | null>(null);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [now, setNow] = useState(() => Date.now());

  const namespace = selectedNamespaces.length === 1 ? selectedNamespaces[0]! : null;

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(id);
  }, []);

  const load = async () => {
    if (!cluster) return;
    setLoading(true);
    try {
      setSummary(await api.gitopsSurvey(cluster, namespace));
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, [cluster, namespace]);

  const visible = useMemo(() => {
    if (!summary) return [];
    const needle = filter.trim().toLowerCase();
    if (!needle) return summary.entries;
    return summary.entries.filter(
      (entry) =>
        entry.name.toLowerCase().includes(needle) ||
        (entry.namespace ?? "").toLowerCase().includes(needle) ||
        (entry.source ?? "").toLowerCase().includes(needle) ||
        entry.status.toLowerCase().includes(needle),
    );
  }, [summary, filter]);

  const act = async (entry: GitOpsEntry, action: () => Promise<void>) => {
    const key = `${entry.resource}/${entry.namespace}/${entry.name}`;
    setBusy(key);
    setError(null);
    try {
      await action();
      await load();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  };

  if (!cluster) {
    return <p className="muted overview__note">Connect a cluster to see what it is syncing.</p>;
  }

  if (summary && summary.controllers.length === 0) {
    return (
      <div className="helm">
        <p className="muted overview__note">
          No GitOps controller found. This screen appears when Argo CD, Flux or Fleet is installed
          — it reads their objects directly, so nothing needs configuring here.
        </p>
      </div>
    );
  }

  return (
    <div className="helm">
      <div className="helm__toolbar">
        <input
          value={filter}
          placeholder="Filter by name, namespace, repository or status"
          onChange={(e) => setFilter(e.target.value)}
        />
        <button className="button" onClick={() => void load()} disabled={loading}>
          {loading ? "Loading…" : "Refresh"}
        </button>
        {summary && (
          <span className="muted">
            {summary.controllers.map((name) => CONTROLLER_LABEL[name] ?? name).join(" · ")}
          </span>
        )}
        {namespace && <span className="muted">namespace: {namespace}</span>}
        {error && <span className="error">{error}</span>}
      </div>

      {summary?.limitations.map((limitation) => (
        <p key={limitation} className="warning sec__note">
          {limitation}
        </p>
      ))}

      <div className="helm__body">
        {visible.length === 0 && !loading ? (
          <p className="muted">Nothing to show.</p>
        ) : (
          <ul className="gitops">
            {visible.map((entry) => {
              const key = `${entry.resource}/${entry.namespace}/${entry.name}`;
              const drifted =
                entry.targetRevision &&
                entry.appliedRevision &&
                !entry.appliedRevision.includes(entry.targetRevision);

              return (
                <li key={key} className={`gitops__row gitops__row--${entry.health}`}>
                  <div className="gitops__head">
                    <span className={`chip chip--${entry.health === "ok" ? "ok" : entry.health}`}>
                      {entry.status}
                    </span>
                    <button
                      className="gitops__name"
                      onClick={() =>
                        void openObject(entry.resource, entry.namespace, entry.name)
                      }
                      title="Open the object"
                    >
                      {entry.name}
                    </button>
                    <span className="muted gitops__meta">
                      {CONTROLLER_LABEL[entry.controller] ?? entry.controller} · {entry.kind}
                      {entry.namespace ? ` · ${entry.namespace}` : ""}
                    </span>
                    <span className="muted gitops__age">{age(entry.lastSync, now)}</span>
                  </div>

                  <div className="gitops__detail">
                    {entry.source && (
                      <span className="gitops__source" title={entry.source}>
                        {entry.source}
                        {entry.path ? ` · ${entry.path}` : ""}
                      </span>
                    )}
                    {entry.appliedRevision && (
                      <span className={drifted ? "warning-text" : "muted"}>
                        {drifted ? "applied" : "at"} {entry.appliedRevision}
                        {drifted && entry.targetRevision ? ` (tracking ${entry.targetRevision})` : ""}
                      </span>
                    )}
                  </div>

                  {entry.message && <p className="gitops__message">{entry.message}</p>}

                  <div className="gitops__actions">
                    {entry.reconcilable && (
                      <button
                        className="button"
                        disabled={busy === key}
                        title="Writes an annotation the controller watches"
                        onClick={() =>
                          void act(entry, () =>
                            api.gitopsReconcile(
                              cluster,
                              entry.resource,
                              entry.namespace,
                              entry.name,
                            ),
                          )
                        }
                      >
                        {busy === key ? "Working…" : "Reconcile now"}
                      </button>
                    )}
                    {entry.controller === "flux" && (
                      <button
                        className="button"
                        disabled={busy === key}
                        onClick={() =>
                          void act(entry, () =>
                            api.gitopsSetSuspended(
                              cluster,
                              entry.resource,
                              entry.namespace,
                              entry.name,
                              !entry.suspended,
                            ),
                          )
                        }
                      >
                        {entry.suspended ? "Resume" : "Suspend"}
                      </button>
                    )}
                    {!entry.reconcilable && (
                      <span className="muted">
                        {CONTROLLER_LABEL[entry.controller]} reconciles on its own schedule.
                      </span>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
