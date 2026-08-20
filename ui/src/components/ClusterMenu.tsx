import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import type { ClusterProfile, ContextEntry } from "../types";

interface Props {
  context: ContextEntry;
  connected: boolean;
  position: { x: number; y: number };
  onClose: () => void;
  onSettings: () => void;
  onRemove: () => void;
}

/** Right-click menu for a cluster tile. */
export function ClusterMenu({
  context,
  connected,
  position,
  onClose,
  onSettings,
  onRemove,
}: Props) {
  const connect = useStore((s) => s.connect);
  const disconnect = useStore((s) => s.disconnect);
  const menu = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const dismiss = (event: MouseEvent) => {
      if (!menu.current?.contains(event.target as Node)) onClose();
    };
    const escape = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    document.addEventListener("mousedown", dismiss);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("mousedown", dismiss);
      document.removeEventListener("keydown", escape);
    };
  }, [onClose]);

  const run = (action: () => void) => {
    action();
    onClose();
  };

  return (
    <div
      className="ctxmenu"
      ref={menu}
      style={{ left: position.x, top: position.y }}
      role="menu"
    >
      <div className="ctxmenu__title">{context.name}</div>

      {connected ? (
        <button
          className="ctxmenu__item"
          onClick={() => run(() => void disconnect(context.name))}
        >
          Disconnect
          <span className="ctxmenu__hint">closes watches, logs, terminals and forwards</span>
        </button>
      ) : (
        <button className="ctxmenu__item" onClick={() => run(() => void connect(context.name))}>
          Connect
        </button>
      )}

      <button className="ctxmenu__item" onClick={() => run(onSettings)}>
        Cluster settings…
      </button>

      <div className="ctxmenu__separator" />

      <button className="ctxmenu__item ctxmenu__item--danger" onClick={() => run(onRemove)}>
        Remove cluster
      </button>
    </div>
  );
}

/** Settings for one cluster: identity, connection options, appearance. */
export function ClusterSettings({
  context,
  onClose,
}: {
  context: ContextEntry;
  onClose: () => void;
}) {
  const clusters = useStore((s) => s.clusters);
  const loadPreferences = useStore((s) => s.loadPreferences);
  const connect = useStore((s) => s.connect);
  const disconnect = useStore((s) => s.disconnect);

  const [profile, setProfile] = useState<ClusterProfile | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    void api
      .clusterProfile(context.name)
      .then(setProfile)
      .catch((err) => setError(String(err)));
  }, [context.name]);

  if (!profile) return null;

  const summary = clusters[context.name];
  const patch = (change: Partial<ClusterProfile>) => setProfile({ ...profile, ...change });

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.setClusterProfile(context.name, profile);
      await loadPreferences();
      // Connection options only take effect on a fresh connection.
      if (summary) {
        setStatus("Saved. Reconnecting so the new options apply…");
        await disconnect(context.name);
        await connect(context.name);
      }
      setStatus("Saved.");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal" role="dialog">
      <div className="modal__card modal__card--wide">
        <h3>{profile.displayName || context.name}</h3>

        <dl className="props">
          <div className="props__row">
            <dt>Context</dt>
            <dd>{context.name}</dd>
          </div>
          <div className="props__row">
            <dt>Server</dt>
            <dd className="muted">{context.server ?? "unknown"}</dd>
          </div>
          <div className="props__row">
            <dt>User</dt>
            <dd>{context.user || "—"}</dd>
          </div>
          <div className="props__row">
            <dt>Authentication</dt>
            <dd>
              {context.execCommand ? (
                <>
                  exec plugin <code>{context.execCommand}</code>
                  {context.missingExecPlugin && (
                    <span className="error"> · not found on PATH</span>
                  )}
                </>
              ) : (
                "certificate or token from the kubeconfig"
              )}
            </dd>
          </div>
          <div className="props__row">
            <dt>Status</dt>
            <dd>
              {summary
                ? summary.status.state === "connected"
                  ? `connected · ${summary.status.version}`
                  : summary.status.state
                : "not connected"}
            </dd>
          </div>
        </dl>

        <section className="context__block">
          <h3>Appearance</h3>
          <div className="field">
            <label className="field__label">
              Display name
              <span className="field__help">
                Shown in the app only. The context keeps its real name, so this and kubectl still
                agree on what the cluster is called.
              </span>
            </label>
            <input
              value={profile.displayName ?? ""}
              placeholder={context.name}
              onChange={(e) => patch({ displayName: e.target.value || null })}
            />
          </div>

          <div className="field">
            <label className="field__label">
              Accent colour
              <span className="field__help">
                Making production look different is the cheapest guard against acting on the wrong
                cluster.
              </span>
            </label>
            <div className="swatches">
              {[null, "#f87171", "#fbbf24", "#34d399", "#38bdf8", "#e879f9"].map((colour) => (
                <button
                  key={colour ?? "none"}
                  className={`swatch-button${profile.colour === colour ? " swatch-button--on" : ""}`}
                  style={colour ? { background: colour } : undefined}
                  title={colour ?? "no accent"}
                  onClick={() => patch({ colour })}
                >
                  {colour ? "" : "—"}
                </button>
              ))}
            </div>
          </div>
        </section>

        <section className="context__block">
          <h3>Connection</h3>
          <p className="muted settings__help">
            These apply the next time the cluster connects. Saving while connected reconnects it.
          </p>

          <div className="field">
            <label className="field__label">
              Default namespace
              <span className="field__help">Overrides whatever the context specifies.</span>
            </label>
            <input
              value={profile.defaultNamespace ?? ""}
              placeholder={context.namespace ?? "default"}
              onChange={(e) => patch({ defaultNamespace: e.target.value || null })}
            />
          </div>

          <div className="field">
            <label className="field__label">
              Act as user
              <span className="field__help">
                Kubernetes impersonation (<code>kubectl --as</code>). Useful for checking what a
                service account can actually do.
              </span>
            </label>
            <input
              value={profile.impersonateUser ?? ""}
              placeholder="system:serviceaccount:default:my-app"
              onChange={(e) => patch({ impersonateUser: e.target.value || null })}
            />
          </div>

          <div className="field">
            <label className="field__label">Act as groups (comma separated)</label>
            <input
              value={profile.impersonateGroups.join(", ")}
              onChange={(e) =>
                patch({
                  impersonateGroups: e.target.value
                    .split(",")
                    .map((group) => group.trim())
                    .filter(Boolean),
                })
              }
            />
          </div>

          <div className="field">
            <label className="field__label">
              Proxy
              <span className="field__help">HTTP or SOCKS5 URL, for this cluster only.</span>
            </label>
            <input
              value={profile.proxyUrl ?? ""}
              placeholder="socks5://localhost:1080"
              onChange={(e) => patch({ proxyUrl: e.target.value || null })}
            />
          </div>

          <label className="checkbox">
            <input
              type="checkbox"
              checked={profile.acceptInvalidCerts}
              onChange={(e) => patch({ acceptInvalidCerts: e.target.checked })}
            />
            Skip TLS verification
          </label>
          {profile.acceptInvalidCerts && (
            <p className="warning">
              The server's identity will not be checked, so anything that can intercept the
              connection can impersonate the cluster and read every credential sent to it. Use
              this only against a cluster you control on a network you trust.
            </p>
          )}
        </section>

        {error && <p className="error">{error}</p>}
        {status && !error && <p className="muted">{status}</p>}

        <div className="modal__actions">
          <button className="button" onClick={onClose}>
            Close
          </button>
          <button className="button button--primary" disabled={busy} onClick={() => void save()}>
            {busy ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
