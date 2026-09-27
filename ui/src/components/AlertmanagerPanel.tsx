import { useEffect, useState } from "react";
import { api } from "../api";
import type { Alert } from "../types";

const SEVERITY_TONE: Record<string, string> = {
  critical: "issues__item--error",
  warning: "issues__item--warning",
};

const DURATIONS = [
  { label: "30m", ms: 30 * 60_000 },
  { label: "1h", ms: 60 * 60_000 },
  { label: "4h", ms: 4 * 60 * 60_000 },
  { label: "1d", ms: 24 * 60 * 60_000 },
];

/** Active Alertmanager alerts, if a cluster has one — silence/unsilence inline. */
export function AlertmanagerPanel({ cluster }: { cluster: string }) {
  const [alerts, setAlerts] = useState<Alert[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [silencing, setSilencing] = useState<string | null>(null);
  const [comment, setComment] = useState("");
  const [duration, setDuration] = useState(DURATIONS[1]!.ms);
  const [busy, setBusy] = useState(false);

  const refresh = () =>
    api
      .alertmanagerAlerts(cluster)
      .then(setAlerts)
      .catch((err) => setError(String(err)));

  useEffect(() => {
    setAlerts(null);
    setError(null);
    void refresh();
    const id = window.setInterval(() => void refresh(), 15_000);
    return () => window.clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cluster]);

  const silence = async (alert: Alert) => {
    setBusy(true);
    try {
      await api.silenceAlert(cluster, {
        matcherName: "alertname",
        matcherValue: alert.labels.alertname ?? "",
        endsAt: new Date(Date.now() + duration).toISOString(),
        createdBy: "kubernaut",
        comment: comment || "silenced from Kubernaut",
      });
      setSilencing(null);
      setComment("");
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const unsilence = async (id: string) => {
    setBusy(true);
    try {
      await api.deleteSilence(cluster, id);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  if (error) return <p className="error overview__note">{error}</p>;
  if (!alerts) return <p className="muted overview__note">Loading…</p>;

  if (alerts.length === 0) {
    return (
      <section className="issues issues--none">
        <div className="issues__tick">✓</div>
        <strong>No active alerts</strong>
        <p className="muted">
          Either the cluster has nothing to page on, or no Alertmanager was found — reachable the
          same way as Prometheus, through the apiserver proxy.
        </p>
      </section>
    );
  }

  return (
    <section className="issues">
      <header className="issues__head">
        <strong>
          {alerts.length} alert{alerts.length === 1 ? "" : "s"}
        </strong>
      </header>
      <ul className="issues__list">
        {alerts.map((alert) => {
          const name = alert.labels.alertname ?? "alert";
          const severity = alert.labels.severity ?? "";
          const summary =
            alert.annotations.summary ?? alert.annotations.description ?? "no summary";
          const tone = SEVERITY_TONE[severity] ?? "";

          return (
            <li key={alert.fingerprint}>
              <div className={`issues__item ${tone}`}>
                <span className="issues__kind">{name}</span>
                <span className="issues__message">{summary}</span>
                <div className="actions__row">
                  {alert.silencedBy.length > 0 ? (
                    alert.silencedBy.map((id) => (
                      <button
                        key={id}
                        className="button"
                        disabled={busy}
                        onClick={() => void unsilence(id)}
                      >
                        Unsilence
                      </button>
                    ))
                  ) : (
                    <button
                      className="button"
                      onClick={() => setSilencing(alert.fingerprint)}
                    >
                      Silence
                    </button>
                  )}
                </div>
                {silencing === alert.fingerprint && (
                  <div className="field field--wide">
                    <select
                      value={duration}
                      onChange={(e) => setDuration(Number(e.target.value))}
                    >
                      {DURATIONS.map((d) => (
                        <option key={d.label} value={d.ms}>
                          {d.label}
                        </option>
                      ))}
                    </select>
                    <input
                      value={comment}
                      placeholder="Reason (optional)"
                      onChange={(e) => setComment(e.target.value)}
                    />
                    <button
                      className="button button--primary"
                      disabled={busy}
                      onClick={() => void silence(alert)}
                    >
                      Confirm
                    </button>
                    <button className="button" onClick={() => setSilencing(null)}>
                      Cancel
                    </button>
                  </div>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
