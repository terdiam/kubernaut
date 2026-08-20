import { useEffect, useState } from "react";
import { api } from "../api";
import type { ForwardStatus } from "../types";

function human(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** Bottom dock listing every live forward, with traffic counters. */
export function ForwardsPanel({ onClose }: { onClose: () => void }) {
  const [forwards, setForwards] = useState<ForwardStatus[]>([]);

  useEffect(() => {
    const refresh = () => void api.listForwards().then(setForwards).catch(() => {});
    refresh();
    const id = window.setInterval(refresh, 1000);
    return () => window.clearInterval(id);
  }, []);

  return (
    <section className="dock">
      <header className="dock__head">
        <strong>Port forwards</strong>
        <span className="muted">{forwards.length} active</span>
        <button className="icon-button" onClick={onClose}>
          ✕
        </button>
      </header>

      {forwards.length === 0 ? (
        <p className="muted dock__empty">
          None running. Start one from a pod or service in the resource table.
        </p>
      ) : (
        <table className="dock__table">
          <thead>
            <tr>
              <th>Local</th>
              <th>Target</th>
              <th>Remote</th>
              <th>Conns</th>
              <th>Sent</th>
              <th>Received</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {forwards.map((forward) => (
              <tr key={forward.id}>
                <td>
                  <a
                    href={`http://${forward.localAddress}:${forward.localPort}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {forward.localAddress}:{forward.localPort}
                  </a>
                </td>
                <td>
                  {forward.namespace}/{forward.name}
                </td>
                <td>{forward.remotePort}</td>
                <td>{forward.activeConnections}</td>
                <td>{human(forward.bytesSent)}</td>
                <td>{human(forward.bytesReceived)}</td>
                <td>
                  <button
                    className="icon-button"
                    title="Stop"
                    onClick={() => void api.stopForward(forward.id)}
                  >
                    ✕
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {forwards.some((f) => f.lastError) && (
        <p className="dock__error">
          {forwards.find((f) => f.lastError)?.lastError}
        </p>
      )}
    </section>
  );
}

interface DialogProps {
  cluster: string;
  resource: string;
  namespace: string;
  name: string;
  onClose: () => void;
  onStarted: () => void;
}

/** Port picker shown when starting a forward from the table. */
export function ForwardDialog({
  cluster,
  resource,
  namespace,
  name,
  onClose,
  onStarted,
}: DialogProps) {
  const [ports, setPorts] = useState<{ port: number; name: string | null }[]>([]);
  const [remotePort, setRemotePort] = useState<number | null>(null);
  const [localPort, setLocalPort] = useState<string>("");
  const [expose, setExpose] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .targetPorts(cluster, resource, namespace, name)
      .then((list) => {
        setPorts(list);
        setRemotePort(list[0]?.port ?? null);
      })
      .catch((err) => setError(String(err)));
  }, [cluster, resource, namespace, name]);

  const start = async () => {
    if (remotePort === null) return;
    setBusy(true);
    setError(null);
    try {
      await api.startForward(cluster, {
        namespace,
        resource,
        name,
        remotePort,
        localPort: localPort === "" ? null : Number(localPort),
        exposeOnNetwork: expose,
      });
      onStarted();
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal" role="dialog">
      <div className="modal__card">
        <h3>Forward {name}</h3>

        <div className="field">
          <label className="field__label">Remote port</label>
          {ports.length > 0 ? (
            <select
              value={remotePort ?? ""}
              onChange={(e) => setRemotePort(Number(e.target.value))}
            >
              {ports.map((port) => (
                <option key={port.port} value={port.port}>
                  {port.port}
                  {port.name ? ` (${port.name})` : ""}
                </option>
              ))}
            </select>
          ) : (
            <input
              type="number"
              value={remotePort ?? ""}
              onChange={(e) => setRemotePort(Number(e.target.value))}
            />
          )}
        </div>

        <div className="field">
          <label className="field__label">
            Local port
            <span className="field__help">Leave blank to pick a free port.</span>
          </label>
          <input value={localPort} onChange={(e) => setLocalPort(e.target.value)} />
        </div>

        <label className="checkbox" title="Binds 0.0.0.0 instead of loopback">
          <input type="checkbox" checked={expose} onChange={(e) => setExpose(e.target.checked)} />
          Expose on the local network
        </label>
        {expose && (
          <p className="warning">
            Anyone on your network will be able to reach this in-cluster service.
          </p>
        )}

        {error && <p className="error">{error}</p>}

        <div className="modal__actions">
          <button className="button" onClick={onClose}>
            Cancel
          </button>
          <button
            className="button button--primary"
            onClick={() => void start()}
            disabled={busy || remotePort === null}
          >
            Start
          </button>
        </div>
      </div>
    </div>
  );
}
