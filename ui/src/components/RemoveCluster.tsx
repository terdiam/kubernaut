import { useEffect, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import type { ContextEntry, ManagedKubeconfig } from "../types";

/**
 * Remove a cluster from the app.
 *
 * The cluster itself is untouched — this forgets the kubeconfig entry. Where a
 * file holds several contexts, removing it takes all of them, which is said
 * before rather than discovered after.
 */
export function RemoveCluster({
  context,
  onClose,
}: {
  context: ContextEntry;
  onClose: () => void;
}) {
  const setContexts = useStore((s) => s.setContexts);
  const disconnect = useStore((s) => s.disconnect);
  const clusters = useStore((s) => s.clusters);

  const [file, setFile] = useState<ManagedKubeconfig | null>(null);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void api
      .managedKubeconfigs()
      .then((files) => {
        setFile(files.find((entry) => entry.contexts.includes(context.name)) ?? null);
      })
      .catch((err) => setError(String(err)));
  }, [context.name]);

  const others = (file?.contexts ?? []).filter((name) => name !== context.name);

  const remove = async () => {
    if (!file) return;
    setBusy(true);
    setError(null);
    try {
      if (clusters[context.name]) await disconnect(context.name);
      setContexts(await api.removeKubeconfig(file.file));
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
        <h3>Remove {context.name}</h3>

        {!file ? (
          <p className="muted">Looking for the kubeconfig that defines it…</p>
        ) : (
          <>
            <p>
              Forgets the kubeconfig stored in this app. The cluster itself is untouched, and
              nothing is deleted from it.
            </p>
            {others.length > 0 && (
              <p className="warning">
                That file also defines {others.join(", ")}. Removing it removes those too — import
                them again separately if you need only some.
              </p>
            )}
            <div className="field">
              <label className="field__label">
                Type <code>{context.name}</code> to confirm
              </label>
              <input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus />
            </div>
          </>
        )}

        {error && <p className="error">{error}</p>}

        <div className="modal__actions">
          <button className="button" onClick={onClose}>
            Cancel
          </button>
          <button
            className="button button--danger"
            disabled={busy || !file || typed !== context.name}
            onClick={() => void remove()}
          >
            {busy ? "Removing…" : "Remove"}
          </button>
        </div>
      </div>
    </div>
  );
}
