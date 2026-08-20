import { useEffect, useState } from "react";
import { api } from "../api";
import type { HelmRelease, HelmReleaseDetail as Detail, HelmRevision, UpgradeDiff } from "../types";
import { HelmDiff } from "./HelmDiff";
import { statusTone } from "./HelmReleases";

interface Props {
  cluster: string;
  release: HelmRelease;
  onClose: () => void;
  onChanged: () => void;
}

type Tab = "values" | "manifest" | "notes" | "history";

export function HelmReleaseDetail({ cluster, release, onClose, onChanged }: Props) {
  const [tab, setTab] = useState<Tab>("values");
  const [detail, setDetail] = useState<Detail | null>(null);
  const [history, setHistory] = useState<HelmRevision[]>([]);
  const [showAllValues, setShowAllValues] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [pending, setPending] = useState<"upgrade" | "uninstall" | number | null>(null);

  useEffect(() => {
    let cancelled = false;
    setDetail(null);
    setError(null);
    setStatus(null);

    void Promise.all([
      api.helmReleaseDetail(cluster, release.namespace, release.name, null),
      api.helmHistory(cluster, release.namespace, release.name),
    ])
      .then(([loaded, revisions]) => {
        if (cancelled) return;
        setDetail(loaded);
        setHistory(revisions);
      })
      .catch((err) => !cancelled && setError(String(err)));

    return () => {
      cancelled = true;
    };
  }, [cluster, release.namespace, release.name]);

  const run = async (action: () => Promise<string>, done: string) => {
    setBusy(true);
    setError(null);
    try {
      const output = await action();
      setStatus(`${done}${output.trim() ? `\n${output.trim()}` : ""}`);
      setPending(null);
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const values = showAllValues ? detail?.effectiveValues : detail?.userValues;

  return (
    <aside className="drawer drawer--wide">
      <header className="drawer__head">
        <div>
          <h2>{release.name}</h2>
          <p className="muted">
            {release.chart} {release.chartVersion} · {release.namespace} · revision{" "}
            {release.revision}{" "}
            <span className={`chip chip--${statusTone(release.status)}`}>{release.status}</span>
          </p>
        </div>
        <button className="drawer__close" onClick={onClose} title="Close">
          ✕
        </button>
      </header>

      <div className="helm__actions">
        <button className="button" onClick={() => setPending("upgrade")}>
          Upgrade
        </button>
        <button
          className="button button--danger"
          onClick={() => setPending("uninstall")}
        >
          Uninstall
        </button>
        {busy && <span className="muted">working…</span>}
      </div>

      {error && <p className="error drawer__body">{error}</p>}
      {status && <pre className="helm__status">{status}</pre>}

      <nav className="drawer__tabs">
        {(["values", "manifest", "notes", "history"] as Tab[]).map((entry) => (
          <button
            key={entry}
            className={`tab${tab === entry ? " tab--active" : ""}`}
            onClick={() => setTab(entry)}
          >
            {entry === "values"
              ? "Values"
              : entry === "manifest"
                ? "Manifest"
                : entry === "notes"
                  ? "Notes"
                  : `History (${history.length})`}
          </button>
        ))}
      </nav>

      {!detail && !error && <p className="muted drawer__body">Loading…</p>}

      {detail && tab === "values" && (
        <>
          <div className="helm__subbar">
            <label className="checkbox">
              <input
                type="checkbox"
                checked={showAllValues}
                onChange={(e) => setShowAllValues(e.target.checked)}
              />
              Include chart defaults
            </label>
            <span className="muted">
              {showAllValues
                ? "Defaults merged with your overrides — what actually rendered."
                : "Only the values supplied at install or upgrade."}
            </span>
          </div>
          <pre className="drawer__yaml">
            {values && values.trim() !== ""
              ? values
              : "# No values were supplied; the chart defaults were used unchanged."}
          </pre>
        </>
      )}

      {detail && tab === "manifest" && <pre className="drawer__yaml">{detail.manifest}</pre>}

      {detail && tab === "notes" && (
        <pre className="drawer__yaml">
          {detail.notes.trim() !== "" ? detail.notes : "# This chart ships no NOTES.txt."}
        </pre>
      )}

      {detail && tab === "history" && (
        <div className="drawer__body">
          <table className="helm__table">
            <thead>
              <tr>
                <th>Rev</th>
                <th>Status</th>
                <th>Chart</th>
                <th>App</th>
                <th>Description</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {history.map((revision) => (
                <tr key={revision.revision}>
                  <td className="helm__num">{revision.revision}</td>
                  <td>
                    <span className={`chip chip--${statusTone(revision.status)}`}>
                      {revision.status}
                    </span>
                  </td>
                  <td>{revision.chartVersion}</td>
                  <td>{revision.appVersion ?? "—"}</td>
                  <td className="muted">{revision.description ?? ""}</td>
                  <td>
                    {revision.revision !== release.revision && (
                      <button
                        className="button"
                        onClick={() => setPending(revision.revision)}
                      >
                        Roll back
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {history.length <= 1 && (
            <p className="muted">
              Only one revision is stored, so there is nothing to roll back to.
            </p>
          )}
        </div>
      )}

      {pending === "upgrade" && detail && (
        <UpgradeDialog
          cluster={cluster}
          release={release}
          initialValues={detail.userValues}
          onCancel={() => setPending(null)}
          onDone={(message) => {
            setStatus(message);
            setPending(null);
            onChanged();
          }}
        />
      )}

      {pending === "uninstall" && (
        <ConfirmRelease
          title="Uninstall release"
          detail={`Removes every object ${release.name} created in ${release.namespace}.`}
          name={release.name}
          verb="Uninstall"
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={(confirmation, keepHistory) =>
            void run(
              () =>
                api.helmUninstall(
                  cluster,
                  release.namespace,
                  release.name,
                  confirmation,
                  keepHistory,
                ),
              "Uninstalled.",
            )
          }
          keepHistoryOption
        />
      )}

      {typeof pending === "number" && (
        <ConfirmRelease
          title={`Roll back to revision ${pending}`}
          detail={`Re-applies the manifest stored for revision ${pending} and creates a new revision.`}
          name={release.name}
          verb="Roll back"
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={(confirmation) =>
            void run(
              () =>
                api.helmRollback(
                  cluster,
                  release.namespace,
                  release.name,
                  pending,
                  confirmation,
                ),
              `Rolled back to revision ${pending}.`,
            )
          }
        />
      )}
    </aside>
  );
}

function UpgradeDialog({
  cluster,
  release,
  initialValues,
  onCancel,
  onDone,
}: {
  cluster: string;
  release: HelmRelease;
  initialValues: string;
  onCancel: () => void;
  onDone: (message: string) => void;
}) {
  const [chart, setChart] = useState(release.chart);
  const [version, setVersion] = useState(release.chartVersion);
  const [values, setValues] = useState(initialValues);
  const [atomic, setAtomic] = useState(true);
  const [diff, setDiff] = useState<UpgradeDiff | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const request = () => ({
    cluster,
    namespace: release.namespace,
    release: release.name,
    chart,
    version: version.trim() === "" ? null : version,
    values,
    options: {
      createNamespace: false,
      wait: false,
      atomic,
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

  const apply = async () => {
    setBusy(true);
    setError(null);
    try {
      const output = await api.helmUpgrade(request());
      onDone(output.trim() || "Upgrade complete.");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal" role="dialog">
      <div className="modal__card modal__card--wide">
        <h3>Upgrade {release.name}</h3>

        <div className="field">
          <label className="field__label">
            Chart
            <span className="field__help">
              `repo/chart`, a local path, or an `oci://` reference. The repo must be configured.
            </span>
          </label>
          <input value={chart} onChange={(e) => setChart(e.target.value)} />
        </div>

        <div className="field">
          <label className="field__label">Version (blank for latest)</label>
          <input value={version} onChange={(e) => setVersion(e.target.value)} />
        </div>

        <div className="field field--wide">
          <label className="field__label">
            Values
            <span className="field__help">
              These replace the values of the current release, not merge into them.
            </span>
          </label>
          <textarea
            rows={10}
            value={values}
            onChange={(e) => setValues(e.target.value)}
            placeholder="# empty means chart defaults"
          />
        </div>

        <label className="checkbox" title="Roll back automatically if the upgrade fails">
          <input type="checkbox" checked={atomic} onChange={(e) => setAtomic(e.target.checked)} />
          Atomic — roll back if the upgrade fails
        </label>

        {error && <p className="error">{error}</p>}

        {diff && <HelmDiff diff={diff} />}

        <div className="modal__actions">
          <button className="button" onClick={onCancel}>
            Cancel
          </button>
          <button className="button" disabled={busy} onClick={() => void preview()}>
            Preview changes
          </button>
          <button
            className="button button--primary"
            disabled={busy || diff === null}
            title={diff === null ? "Preview the changes first" : undefined}
            onClick={() => void apply()}
          >
            Upgrade
          </button>
        </div>
      </div>
    </div>
  );
}

function ConfirmRelease({
  title,
  detail,
  name,
  verb,
  busy,
  keepHistoryOption,
  onCancel,
  onConfirm,
}: {
  title: string;
  detail: string;
  name: string;
  verb: string;
  busy: boolean;
  keepHistoryOption?: boolean;
  onCancel: () => void;
  onConfirm: (confirmation: string, keepHistory: boolean) => void;
}) {
  const [typed, setTyped] = useState("");
  const [keepHistory, setKeepHistory] = useState(false);

  return (
    <div className="modal" role="dialog">
      <div className="modal__card">
        <h3>{title}</h3>
        <p className="muted">{detail}</p>
        <div className="field">
          <label className="field__label">
            Type <code>{name}</code> to confirm
          </label>
          <input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus />
        </div>
        {keepHistoryOption && (
          <label className="checkbox">
            <input
              type="checkbox"
              checked={keepHistory}
              onChange={(e) => setKeepHistory(e.target.checked)}
            />
            Keep release history (allows a later rollback)
          </label>
        )}
        <div className="modal__actions">
          <button className="button" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="button button--danger"
            disabled={busy || typed !== name}
            onClick={() => onConfirm(typed, keepHistory)}
          >
            {verb}
          </button>
        </div>
      </div>
    </div>
  );
}
