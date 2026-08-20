import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { age } from "../age";
import { useStore } from "../store";
import { toneForValue } from "../statusTone";
import { HelmReleaseDetail } from "./HelmReleaseDetail";
import type { HelmRelease } from "../types";

/**
 * Every Helm release in the cluster.
 *
 * Read from the release Secrets rather than from `helm list`, so releases
 * installed by CI, Flux, Rancher or a colleague appear too — the set a UI that
 * only tracked its own installs would quietly miss.
 */
export function HelmReleases() {
  const cluster = useStore((s) => s.activeCluster);
  const namespaces = useStore((s) => s.selectedNamespaces);

  const [releases, setReleases] = useState<HelmRelease[]>([]);
  const [selected, setSelected] = useState<HelmRelease | null>(null);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [reload, setReload] = useState(0);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(id);
  }, []);

  useEffect(() => {
    if (!cluster) return;
    let cancelled = false;
    setLoading(true);
    // One namespace selected narrows the query; several are filtered here,
    // because the release store lists per namespace or cluster-wide only.
    const scope = namespaces.length === 1 ? namespaces[0]! : null;

    api
      .helmReleases(cluster, scope)
      .then((list) => {
        if (cancelled) return;
        setReleases(
          namespaces.length > 1
            ? list.filter((release) => namespaces.includes(release.namespace))
            : list,
        );
        setError(null);
      })
      .catch((err) => !cancelled && setError(String(err)))
      .finally(() => !cancelled && setLoading(false));

    return () => {
      cancelled = true;
    };
  }, [cluster, namespaces.join(","), reload]);

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return releases;
    return releases.filter(
      (release) =>
        release.name.toLowerCase().includes(needle) ||
        release.namespace.toLowerCase().includes(needle) ||
        release.chart.toLowerCase().includes(needle),
    );
  }, [releases, filter]);

  if (!cluster) {
    return <p className="muted overview__note">Connect a cluster to see its releases.</p>;
  }

  return (
    <div className="helm">
      <div className="helm__toolbar">
        <input
          value={filter}
          placeholder={`Filter ${releases.length} releases`}
          onChange={(e) => setFilter(e.target.value)}
        />
        <button className="button" onClick={() => setReload((n) => n + 1)}>
          Refresh
        </button>
        {loading && <span className="muted">loading…</span>}
        {error && <span className="error">{error}</span>}
      </div>

      <div className="helm__body">
        <table className="helm__table">
          <thead>
            <tr>
              <th>Release</th>
              <th>Namespace</th>
              <th>Chart</th>
              <th>Version</th>
              <th>App</th>
              <th>Rev</th>
              <th>Status</th>
              <th>Updated</th>
            </tr>
          </thead>
          <tbody>
            {visible.map((release) => (
              <tr
                key={`${release.namespace}/${release.name}`}
                className={
                  selected?.name === release.name && selected?.namespace === release.namespace
                    ? "helm__row helm__row--selected"
                    : "helm__row"
                }
                onClick={() => setSelected(release)}
              >
                <td className="helm__name">{release.name}</td>
                <td>{release.namespace}</td>
                <td>{release.chart}</td>
                <td>{release.chartVersion}</td>
                <td>{release.appVersion ?? "—"}</td>
                <td className="helm__num">{release.revision}</td>
                <td>
                  <span className={`chip chip--${statusTone(release.status)}`}>
                    {release.status}
                  </span>
                </td>
                <td className="muted">{age(release.updated, now)}</td>
              </tr>
            ))}
          </tbody>
        </table>

        {visible.length === 0 && !loading && (
          <p className="muted overview__note">
            {releases.length === 0
              ? "No Helm releases in this cluster."
              : "No release matches the filter."}
          </p>
        )}
      </div>

      {selected && (
        <HelmReleaseDetail
          cluster={cluster}
          release={selected}
          onClose={() => setSelected(null)}
          onChanged={() => {
            setSelected(null);
            setReload((n) => n + 1);
          }}
        />
      )}
    </div>
  );
}

/** Helm release states share the shared status vocabulary. */
export function statusTone(status: string): string {
  return toneForValue(status) ?? "unknown";
}
