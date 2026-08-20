import { useEffect, useRef, useState } from "react";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api";
import { api } from "../api";
import type { DiffResult } from "../types";

interface Props {
  cluster: string;
  resource: string;
  namespace: string | null;
  name: string;
  /** Server-rendered YAML for the live object. */
  initial: string;
  onApplied: () => void;
}

/**
 * YAML editing with cluster-accurate validation, a real dry-run diff before
 * saving, and explicit handling of field-manager conflicts.
 */
export function YamlEditor({
  cluster,
  resource,
  namespace,
  name,
  initial,
  onApplied,
}: Props) {
  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<Monaco.editor.IStandaloneCodeEditor | null>(null);
  const [diff, setDiff] = useState<DiffResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [force, setForce] = useState(false);

  // Monaco is several megabytes; loading it on demand keeps app startup fast
  // for the common case where nobody opens the YAML tab.
  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | null = null;

    void (async () => {
      const { applySchema, editorTheme, modelUriFor, monaco } = await import("../monaco");
      if (disposed || !host.current) return;

      const uri = modelUriFor(resource, name);
      const existing = monaco.editor.getModel(uri);
      const model = existing ?? monaco.editor.createModel(initial, "yaml", uri);
      if (existing) existing.setValue(initial);

      const instance = monaco.editor.create(host.current, {
        model,
        theme: editorTheme(),
        automaticLayout: true,
        minimap: { enabled: false },
        fontSize: 12,
        lineNumbersMinChars: 3,
        scrollBeyondLastLine: false,
        renderWhitespace: "selection",
        tabSize: 2,
        quickSuggestions: { other: true, strings: true },
      });
      editor.current = instance;

      // Schema load is best-effort: an editor without completion is still
      // usable, and some clusters restrict the OpenAPI endpoint.
      void api
        .resourceSchema(cluster, resource)
        .then((payload) => applySchema(resource, payload.schema))
        .catch((err) => setStatus(`schema unavailable: ${err}`));

      // Follow the app theme, including "system" flipping while open.
      const observer = new MutationObserver(() =>
        monaco.editor.setTheme(editorTheme()),
      );
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });
      const media = window.matchMedia("(prefers-color-scheme: dark)");
      const onSchemeChange = () => monaco.editor.setTheme(editorTheme());
      media.addEventListener("change", onSchemeChange);

      cleanup = () => {
        observer.disconnect();
        media.removeEventListener("change", onSchemeChange);
        instance.dispose();
        model.dispose();
        editor.current = null;
      };
    })();

    return () => {
      disposed = true;
      cleanup?.();
    };
  }, [cluster, resource, name, initial]);

  const request = (forceApply: boolean) => ({
    resource,
    namespace,
    name,
    yaml: editor.current?.getValue() ?? initial,
    force: forceApply,
  });

  const preview = async () => {
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const result = await api.previewEdit(cluster, request(force));
      setDiff(result);
      if (!result.changed) setStatus("No changes.");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const outcome = await api.applyEdit(cluster, request(force));
      if (outcome.status === "conflict") {
        const managers = [...new Set(outcome.conflicts.map((c) => c.manager))].join(", ");
        const fields = outcome.conflicts.map((c) => c.field).filter(Boolean);
        setError(
          `Owned by ${managers}: ${fields.join(", ")}. Enable Force to take ownership.`,
        );
        return;
      }
      setDiff(null);
      setStatus("Applied.");
      onApplied();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="editor">
      <div className="editor__toolbar">
        <button className="button" onClick={() => void preview()} disabled={busy}>
          Preview changes
        </button>
        <label className="checkbox" title="Take ownership of fields another manager owns">
          <input
            type="checkbox"
            checked={force}
            onChange={(e) => setForce(e.target.checked)}
          />
          Force
        </label>
        {status && <span className="muted">{status}</span>}
        {error && <span className="error">{error}</span>}
      </div>

      <div className="editor__host" ref={host} />

      {diff && (
        <div className="diff">
          <header className="diff__head">
            <strong>{diff.changed ? "Proposed changes" : "No changes"}</strong>
            <div className="diff__actions">
              {diff.changed && (
                <button className="button button--primary" onClick={() => void save()} disabled={busy}>
                  Apply
                </button>
              )}
              <button className="icon-button" onClick={() => setDiff(null)}>
                ✕
              </button>
            </div>
          </header>

          {diff.conflicts.length > 0 && (
            <div className="diff__conflict">
              <p>
                Owned by {[...new Set(diff.conflicts.map((c) => c.manager))].join(", ")}. Enable{" "}
                <em>Force</em> to take ownership — this can revert what that manager set.
              </p>
              <ul className="conflict__fields">
                {diff.conflicts.map((conflict) => (
                  <li key={`${conflict.manager}-${conflict.field}`}>
                    <code>{conflict.field || "(field not reported)"}</code>
                    <span className="muted"> — {conflict.manager}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {diff.unified && (
            <pre className="diff__body">
              {diff.unified.split("\n").map((line, index) => (
                <span
                  key={index}
                  className={
                    line.startsWith("+")
                      ? "diff__add"
                      : line.startsWith("-")
                        ? "diff__del"
                        : line.startsWith("@@")
                          ? "diff__hunk"
                          : undefined
                  }
                >
                  {line}
                  {"\n"}
                </span>
              ))}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}
