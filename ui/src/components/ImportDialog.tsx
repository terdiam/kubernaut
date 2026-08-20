import { useEffect, useRef, useState } from "react";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api";
import { api } from "../api";
import { useStore } from "../store";
import { PlanTable, ResultTable } from "./ManifestOutcome";
import type { DocResult, ManifestPlan } from "../types";

/**
 * Import a manifest file.
 *
 * Only that: a file the user already has, applied as it stands. Creating a
 * resource from scratch is a different job with a different starting point,
 * and folding the two together made both harder to find.
 */
export function ImportDialog({ onClose }: { onClose: () => void }) {
  const cluster = useStore((s) => s.activeCluster);
  const selectedNamespaces = useStore((s) => s.selectedNamespaces);
  const namespaces = useStore((s) => s.namespaces);

  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<Monaco.editor.IStandaloneCodeEditor | null>(null);

  const [namespace, setNamespace] = useState(selectedNamespaces[0] ?? "default");
  const [force, setForce] = useState(false);
  const [plan, setPlan] = useState<ManifestPlan | null>(null);
  const [results, setResults] = useState<DocResult[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [source, setSource] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | null = null;

    void (async () => {
      const { editorTheme, modelUriFor, monaco } = await import("../monaco");
      if (disposed || !host.current) return;

      const uri = modelUriFor("manifest", "import");
      const model = monaco.editor.getModel(uri) ?? monaco.editor.createModel("", "yaml", uri);
      model.setValue("");

      // No schema is attached: a manifest holds several kinds at once, so no
      // single schema fits. The plan is the real validation — it comes from
      // the apiserver rather than a guess made here.
      const instance = monaco.editor.create(host.current, {
        model,
        theme: editorTheme(),
        automaticLayout: true,
        minimap: { enabled: false },
        fontSize: 12,
        lineNumbersMinChars: 3,
        scrollBeyondLastLine: false,
        tabSize: 2,
      });
      editor.current = instance;

      const observer = new MutationObserver(() => monaco.editor.setTheme(editorTheme()));
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });

      cleanup = () => {
        observer.disconnect();
        instance.dispose();
        model.dispose();
        editor.current = null;
      };
    })();

    return () => {
      disposed = true;
      cleanup?.();
    };
  }, []);

  const openFile = async (file: File | undefined) => {
    if (!file) return;
    try {
      editor.current?.setValue(await file.text());
      setSource(file.name);
      setPlan(null);
      setResults(null);
      setError(null);
    } catch (err) {
      setError(String(err));
    }
  };

  const run = async (apply: boolean) => {
    if (!cluster) return;
    setBusy(true);
    setError(null);
    try {
      const yaml = editor.current?.getValue() ?? "";
      if (apply) {
        setResults(await api.applyManifest(cluster, yaml, namespace || null, force));
        setPlan(null);
      } else {
        setPlan(await api.planManifest(cluster, yaml, namespace || null, force));
        setResults(null);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const blocked = plan?.docs.some((doc) => doc.action === "error") ?? false;
  const conflicted = plan?.docs.some((doc) => doc.action === "conflict") ?? false;
  const done = results?.every((entry) => entry.status !== "error" && entry.status !== "conflict");

  return (
    <div className="modal" role="dialog" aria-label="Import YAML">
      <div className="modal__card modal__card--wide manifest">
        <h3>Import YAML</h3>

        <div className="manifest__toolbar">
          <label className="button button--primary manifest__file">
            Choose a file…
            <input
              type="file"
              accept=".yaml,.yml,.json,text/yaml,application/json"
              onChange={(e) => void openFile(e.target.files?.[0])}
            />
          </label>

          <label className="manifest__ns">
            Default namespace
            <input
              list="import-namespaces"
              value={namespace}
              onChange={(e) => setNamespace(e.target.value)}
              placeholder="default"
            />
            <datalist id="import-namespaces">
              {namespaces.map((entry) => (
                <option key={entry} value={entry} />
              ))}
            </datalist>
          </label>

          <label className="checkbox" title="Take ownership of fields another manager owns">
            <input type="checkbox" checked={force} onChange={(e) => setForce(e.target.checked)} />
            Force
          </label>

          {source && <span className="muted">{source}</span>}
        </div>

        <p className="muted manifest__hint">
          Or paste below. Documents are separated by <code>---</code>; each may set its own{" "}
          <code>metadata.namespace</code>, and the default above only fills the gap for those that
          do not.
        </p>

        <div className="manifest__editor" ref={host} />

        {error && <p className="error">{error}</p>}
        {plan && <PlanTable plan={plan} />}
        {results && <ResultTable results={results} />}

        <div className="modal__actions">
          <button className="button" onClick={onClose}>
            {done ? "Close" : "Cancel"}
          </button>
          <button className="button" onClick={() => void run(false)} disabled={busy || !cluster}>
            Preview
          </button>
          <button
            className="button button--primary"
            onClick={() => void run(true)}
            disabled={busy || !plan || blocked || !cluster}
            title={
              !plan
                ? "Preview first — the plan is checked against the cluster"
                : blocked
                  ? "Fix the documents marked error first"
                  : conflicted
                    ? "Some fields are owned by another manager; enable Force to take them"
                    : undefined
            }
          >
            Apply
          </button>
        </div>
      </div>
    </div>
  );
}
