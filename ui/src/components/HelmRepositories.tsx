import { useEffect, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import { HelmDiff } from "./HelmDiff";
import type { HelmChart, HelmInfo, HelmRepository, UpgradeDiff } from "../types";

/**
 * Chart repositories and search.
 *
 * These write to the user's own helm configuration, so a repo added here also
 * appears in their CLI. That is deliberate: two divergent repo lists on one
 * machine would be worse than sharing one.
 */
export function HelmRepositories() {
  const [info, setInfo] = useState<HelmInfo | null>(null);
  const [repos, setRepos] = useState<HelmRepository[]>([]);
  const [charts, setCharts] = useState<HelmChart[]>([]);
  const [query, setQuery] = useState("");
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [installing, setInstalling] = useState<HelmChart | null>(null);

  const refresh = async () => {
    try {
      const [binary, list] = await Promise.all([api.helmInfo(), api.helmRepos()]);
      setInfo(binary);
      setRepos(list);
      setError(null);
    } catch (err) {
      setError(String(err));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const run = async (action: () => Promise<unknown>, done: string) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      setStatus(done);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const search = async () => {
    setBusy(true);
    setError(null);
    try {
      setCharts(await api.helmSearch(query));
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="helm">
      <div className="helm__toolbar">
        {info ? (
          <span className="muted">
            helm {info.version} · {info.path}
            {info.bundled ? " (bundled)" : ""}
          </span>
        ) : (
          <span className="warning-text">
            No helm binary found. Releases still list and inspect, but installing, upgrading and
            rolling back need helm on PATH.
          </span>
        )}
        <button className="button" disabled={!info || busy} onClick={() => void run(api.helmRepoUpdate, "Repositories updated.")}>
          Update repositories
        </button>
        {status && <span className="muted">{status}</span>}
        {error && <span className="error">{error}</span>}
      </div>

      <div className="helm__body helm__body--split">
        <section>
          <h3 className="helm__heading">Repositories</h3>
          {repos.length === 0 ? (
            <p className="muted">None configured.</p>
          ) : (
            <table className="helm__table">
              <tbody>
                {repos.map((repo) => (
                  <tr key={repo.name}>
                    <td className="helm__name">{repo.name}</td>
                    <td className="muted">{repo.url}</td>
                    <td>
                      <button
                        className="icon-button"
                        title="Remove"
                        disabled={busy}
                        onClick={() =>
                          void run(() => api.helmRepoRemove(repo.name), `Removed ${repo.name}.`)
                        }
                      >
                        ✕
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <div className="helm__addrepo">
            <input
              value={name}
              placeholder="name"
              onChange={(e) => setName(e.target.value)}
            />
            <input
              value={url}
              placeholder="https://charts.example.com"
              onChange={(e) => setUrl(e.target.value)}
            />
            <button
              className="button"
              disabled={!info || busy || name.trim() === "" || url.trim() === ""}
              onClick={() =>
                void run(() => api.helmRepoAdd(name.trim(), url.trim()), `Added ${name}.`).then(
                  () => {
                    setName("");
                    setUrl("");
                  },
                )
              }
            >
              Add
            </button>
          </div>
        </section>

        <section>
          <h3 className="helm__heading">Charts</h3>
          <div className="helm__search">
            <input
              value={query}
              placeholder="Search configured repositories"
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void search()}
            />
            <button className="button" disabled={!info || busy} onClick={() => void search()}>
              Search
            </button>
          </div>

          {charts.length === 0 ? (
            <p className="muted">
              {info
                ? "No results yet. Search only covers repositories configured above."
                : "Search needs the helm binary."}
            </p>
          ) : (
            <table className="helm__table">
              <tbody>
                {charts.map((chart) => (
                  <tr key={`${chart.name}-${chart.version}`}>
                    <td className="helm__name">{chart.name}</td>
                    <td>{chart.version}</td>
                    <td className="muted">{chart.appVersion ?? ""}</td>
                    <td className="muted helm__desc" title={chart.description ?? ""}>
                      {chart.description ?? ""}
                    </td>
                    <td>
                      <button className="button" onClick={() => setInstalling(chart)}>
                        Install
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>

      {installing && (
        <InstallDialog chart={installing} onClose={() => setInstalling(null)} />
      )}
    </div>
  );
}

function InstallDialog({ chart, onClose }: { chart: HelmChart; onClose: () => void }) {
  const cluster = useStore((s) => s.activeCluster);
  const namespaces = useStore((s) => s.namespaces);
  const selectedNamespaces = useStore((s) => s.selectedNamespaces);

  const [release, setRelease] = useState(chart.name.split("/").pop() ?? "");
  const [namespace, setNamespace] = useState(selectedNamespaces[0] ?? "default");
  const [values, setValues] = useState("");
  const [createNamespace, setCreateNamespace] = useState(false);
  const [diff, setDiff] = useState<UpgradeDiff | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  // Start from the chart's own defaults so the editor shows what can be
  // changed, rather than an empty box.
  useEffect(() => {
    void api
      .helmChartValues(chart.name, chart.version)
      .then((defaults) => setValues(defaults))
      .catch(() => setValues(""));
  }, [chart.name, chart.version]);

  if (!cluster) return null;

  const request = () => ({
    cluster,
    namespace,
    release,
    chart: chart.name,
    version: chart.version,
    values,
    options: {
      createNamespace,
      wait: false,
      atomic: true,
      resetValues: false,
      dryRun: false,
      timeoutSeconds: 300,
    },
  });

  const preview = async () => {
    setBusy(true);
    setError(null);
    try {
      setDiff(await api.helmPreviewUpgrade(request()));
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const install = async () => {
    setBusy(true);
    setError(null);
    try {
      const output = await api.helmUpgrade(request());
      setDone(output.trim() || "Installed.");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal" role="dialog">
      <div className="modal__card modal__card--wide">
        <h3>
          Install {chart.name} {chart.version}
        </h3>

        <div className="field">
          <label className="field__label">Release name</label>
          <input value={release} onChange={(e) => setRelease(e.target.value)} />
        </div>

        <div className="field">
          <label className="field__label">Namespace</label>
          <input
            list="kubernaut-namespaces"
            value={namespace}
            onChange={(e) => setNamespace(e.target.value)}
          />
          <datalist id="kubernaut-namespaces">
            {namespaces.map((ns) => (
              <option key={ns} value={ns} />
            ))}
          </datalist>
        </div>

        <label className="checkbox">
          <input
            type="checkbox"
            checked={createNamespace}
            onChange={(e) => setCreateNamespace(e.target.checked)}
          />
          Create the namespace if it does not exist
        </label>

        <div className="field field--wide">
          <label className="field__label">Values</label>
          <textarea rows={12} value={values} onChange={(e) => setValues(e.target.value)} />
        </div>

        {error && <p className="error">{error}</p>}
        {done && <pre className="helm__status">{done}</pre>}

        {diff && <HelmDiff diff={diff} />}

        <div className="modal__actions">
          <button className="button" onClick={onClose}>
            {done ? "Close" : "Cancel"}
          </button>
          <button className="button" disabled={busy || !!done} onClick={() => void preview()}>
            Preview
          </button>
          <button
            className="button button--primary"
            disabled={busy || diff === null || !!done || release.trim() === ""}
            title={diff === null ? "Preview first" : undefined}
            onClick={() => void install()}
          >
            Install
          </button>
        </div>
      </div>
    </div>
  );
}
