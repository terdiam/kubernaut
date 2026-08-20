import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import { useStore } from "../store";
import type { ContextEntry, ImportPreview } from "../types";

type Source = "system" | "file" | "paste";

/**
 * Import a cluster from a kubeconfig.
 *
 * The file is stored in this app's own directory, never merged into
 * `~/.kube/config` — that file belongs to kubectl and everything else on the
 * machine, and a UI that rewrites it can break workflows it knows nothing
 * about. The trade-off is that an imported cluster is visible here and not to
 * kubectl, which the dialog says outright.
 */
export function AddCluster({ onClose }: { onClose: () => void }) {
  const setContexts = useStore((s) => s.setContexts);

  const [yaml, setYaml] = useState("");
  const [label, setLabel] = useState("");
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [renames, setRenames] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [source, setSource] = useState<string | null>(null);
  const [mode, setMode] = useState<Source>("system");
  const [systemContexts, setSystemContexts] = useState<ContextEntry[] | null>(null);
  const [picked, setPicked] = useState<string[]>([]);

  // Reading the user's kubeconfig is not the same as using it: nothing here
  // reaches a cluster until a context is chosen and copied into this app.
  useEffect(() => {
    void api
      .systemKubeconfigContexts()
      .then((found) => {
        setSystemContexts(found);
        if (found.length === 0) setMode("file");
      })
      .catch(() => setSystemContexts([]));
  }, []);

  const inspect = async (text: string, suggestedLabel?: string) => {
    setYaml(text);
    setError(null);
    setPreview(null);
    try {
      const result = await api.previewKubeconfig(text);
      setPreview(result);
      setRenames(result.suggested);
      if (!label) {
        setLabel(suggestedLabel ?? result.contexts[0] ?? "imported");
      }
    } catch (err) {
      setError(String(err));
    }
  };

  const browse = async () => {
    setError(null);
    try {
      const picked = await open({
        multiple: false,
        title: "Choose a kubeconfig",
        filters: [{ name: "kubeconfig", extensions: ["yaml", "yml", "config", "kubeconfig"] }],
      });
      if (typeof picked !== "string") return;
      // Read in Rust: the renderer never needs filesystem access, and the
      // credentials stay on that side of the bridge.
      const text = await api.readKubeconfigFile(picked);
      setSource(picked);
      const name = picked.split(/[\\/]/).pop()?.replace(/\.(ya?ml|config|kubeconfig)$/i, "");
      await inspect(text, name);
    } catch (err) {
      setError(String(err));
    }
  };

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const contexts = await api.importKubeconfig({ yaml, label, renames });
      setContexts(contexts);
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const unresolved = (preview?.conflicts ?? []).filter(
    (name) => !renames[name] || renames[name] === name,
  );

  const importSystem = async () => {
    setBusy(true);
    setError(null);
    try {
      const contexts = await api.importSystemContexts(
        picked,
        picked.length === 1 ? picked[0]! : "kubeconfig",
      );
      setContexts(contexts);
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal" role="dialog">
      <div className="modal__card modal__card--wide">
        <h3>Add a cluster</h3>

        <div className="segmented addcluster__modes">
          {(
            [
              ["system", `From your kubeconfig${systemContexts ? ` (${systemContexts.length})` : ""}`],
              ["file", "From a file"],
              ["paste", "Paste"],
            ] as [Source, string][]
          ).map(([id, label]) => (
            <button
              key={id}
              className={`segmented__button${mode === id ? " segmented__button--active" : ""}`}
              onClick={() => setMode(id)}
            >
              {label}
            </button>
          ))}
        </div>

        {mode === "system" && (
          <>
            <p className="muted settings__help">
              Contexts found in <code>~/.kube/config</code>. This app does not use that file
              directly — pick what you want and a copy is stored here, so nothing is reachable
              just because it is on disk.
            </p>
            {systemContexts === null ? (
              <p className="muted">Reading…</p>
            ) : systemContexts.length === 0 ? (
              <p className="muted">
                No kubeconfig found at <code>~/.kube/config</code>. Use a file or paste instead.
              </p>
            ) : (
              <div className="kv addcluster__contexts">
                {systemContexts.map((entry) => (
                  <label key={entry.name} className="checkbox">
                    <input
                      type="checkbox"
                      checked={picked.includes(entry.name)}
                      onChange={(e) =>
                        setPicked(
                          e.target.checked
                            ? [...picked, entry.name]
                            : picked.filter((name) => name !== entry.name),
                        )
                      }
                    />
                    {entry.name}
                    {entry.server && <span className="muted"> · {entry.server}</span>}
                  </label>
                ))}
              </div>
            )}
          </>
        )}

        {mode === "file" && (
          <div className="addcluster__sources">
            <button className="button" onClick={() => void browse()}>
              Choose a kubeconfig file…
            </button>
          </div>
        )}

        {source && <p className="muted addcluster__source">From {source}</p>}

        {mode !== "system" && (
        <div className="field field--wide">
          <label className="field__label">
            Kubeconfig
            <span className="field__help">
              Stored in this app's own configuration. Your <code>~/.kube/config</code> is not
              modified, so an imported cluster appears here but not in kubectl.
            </span>
          </label>
          <textarea
            rows={10}
            value={yaml}
            spellCheck={false}
            placeholder={"apiVersion: v1\nkind: Config\nclusters: …"}
            onChange={(e) => void inspect(e.target.value)}
          />
        </div>
        )}

        {mode !== "system" && preview && (
          <>
            <div className="field">
              <label className="field__label">Stored as</label>
              <input value={label} onChange={(e) => setLabel(e.target.value)} />
            </div>

            <p className="muted">
              Adds {preview.contexts.length} context
              {preview.contexts.length === 1 ? "" : "s"}: {preview.contexts.join(", ")}
            </p>

            {preview.conflicts.length > 0 && (
              <div className="warning addcluster__conflicts">
                <p>
                  These names already exist. Two contexts with one name give no way to tell which
                  cluster a click reaches, so rename them before importing.
                </p>
                {preview.conflicts.map((name) => (
                  <div className="kv__row" key={name}>
                    <input value={name} disabled />
                    <input
                      value={renames[name] ?? ""}
                      placeholder="new name"
                      onChange={(e) => setRenames({ ...renames, [name]: e.target.value })}
                    />
                  </div>
                ))}
              </div>
            )}
          </>
        )}

        {error && <p className="error">{error}</p>}

        <div className="modal__actions">
          <button className="button" onClick={onClose}>
            Cancel
          </button>
          {mode === "system" ? (
            <button
              className="button button--primary"
              disabled={busy || picked.length === 0}
              onClick={() => void importSystem()}
            >
              {busy ? "Importing…" : `Import ${picked.length || ""}`.trim()}
            </button>
          ) : (
            <button
              className="button button--primary"
              disabled={busy || !preview || unresolved.length > 0 || label.trim() === ""}
              title={unresolved.length > 0 ? "Rename the conflicting contexts first" : undefined}
              onClick={() => void submit()}
            >
              {busy ? "Importing…" : "Import"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
