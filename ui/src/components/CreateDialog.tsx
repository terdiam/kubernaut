import { useEffect, useMemo, useRef, useState } from "react";
import { parse, stringify } from "yaml";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api";
import { api } from "../api";
import { useStore } from "../store";
import { formSections } from "../formSpec";
import { getPath, setPath } from "../path";
import { templateForKind } from "../manifestTemplates";
import { FormContext } from "../formContext";
import { Wizard } from "./Wizard";
import { PlanTable, ResultTable } from "./ManifestOutcome";
import type { DocResult, ManifestPlan, ResourceDescriptor } from "../types";

type Obj = Record<string, unknown>;
type Mode = "form" | "yaml";

/**
 * Create one resource, as a form or as YAML.
 *
 * The two are views of the same draft rather than two editors: switching keeps
 * what was typed, because losing a half-filled form to a toggle is the kind of
 * thing that stops people using the form at all.
 */
export function CreateDialog({ onClose }: { onClose: () => void }) {
  const cluster = useStore((s) => s.activeCluster);
  const current = useStore((s) => s.resource);
  const discovery = useStore((s) => s.discovery);
  const selectedNamespaces = useStore((s) => s.selectedNamespaces);
  const namespaces = useStore((s) => s.namespaces);

  // The page decides what gets created. Opened from somewhere with no resource
  // in view, the dialog asks instead of guessing.
  const [descriptor, setDescriptor] = useState<ResourceDescriptor | null>(
    current?.verbs.includes("create") ? current : null,
  );

  if (!descriptor) {
    return (
      <KindPicker
        discovery={discovery}
        onPick={setDescriptor}
        onClose={onClose}
        suggested={current ?? null}
      />
    );
  }

  return (
    <CreateForm
      key={descriptor.key}
      cluster={cluster}
      descriptor={descriptor}
      namespaces={namespaces}
      defaultNamespace={selectedNamespaces[0] ?? "default"}
      onChangeKind={() => setDescriptor(null)}
      onClose={onClose}
    />
  );
}

function CreateForm({
  cluster,
  descriptor,
  namespaces,
  defaultNamespace,
  onChangeKind,
  onClose,
}: {
  cluster: string | null;
  descriptor: ResourceDescriptor;
  namespaces: string[];
  defaultNamespace: string;
  onChangeKind: () => void;
  onClose: () => void;
}) {
  const sections = useMemo(
    () => formSections(descriptor.group, descriptor.kind),
    [descriptor.group, descriptor.kind],
  );

  const seed = useMemo(
    () => parse(templateForKind(descriptor.apiVersion, descriptor.kind)) as Obj,
    [descriptor.apiVersion, descriptor.kind],
  );

  // Only kinds with a layout can open on the form; the rest have nothing to
  // show there, so opening on YAML is the honest default.
  const [mode, setMode] = useState<Mode>(sections ? "form" : "yaml");
  const [draft, setDraft] = useState<Obj>(seed);
  const [namespace, setNamespace] = useState(descriptor.namespaced ? defaultNamespace : "");
  const [plan, setPlan] = useState<ManifestPlan | null>(null);
  const [results, setResults] = useState<DocResult[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<Monaco.editor.IStandaloneCodeEditor | null>(null);

  useEffect(() => {
    if (mode !== "yaml") return;
    let disposed = false;
    let cleanup: (() => void) | null = null;

    void (async () => {
      const { editorTheme, modelUriFor, monaco } = await import("../monaco");
      if (disposed || !host.current) return;

      const uri = modelUriFor("create", descriptor.key);
      const model = monaco.editor.getModel(uri) ?? monaco.editor.createModel("", "yaml", uri);
      model.setValue(stringify(draft));

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

      cleanup = () => {
        instance.dispose();
        model.dispose();
        editor.current = null;
      };
    })();

    return () => {
      disposed = true;
      cleanup?.();
    };
    // Re-seeding on every draft change would fight the typist; the model is
    // set once per switch into YAML mode.
  }, [mode, descriptor.key]);

  /** The draft as it stands, wherever it is being edited. */
  const currentDraft = (): Obj => {
    if (mode !== "yaml") return draft;
    const text = editor.current?.getValue() ?? "";
    return (parse(text) ?? {}) as Obj;
  };

  const switchMode = (next: Mode) => {
    setError(null);
    if (next === mode) return;
    if (mode === "yaml") {
      // Carry the edited text back, or refuse — silently dropping it would be
      // worse than staying put.
      try {
        setDraft(currentDraft());
      } catch (err) {
        setError(`The YAML has to parse before switching to the form: ${err}`);
        return;
      }
    }
    setMode(next);
  };

  const update = (path: string, value: unknown) =>
    setDraft((current) => setPath(current, path, value));

  /**
   * Rename, and carry the labels that still match along with it.
   *
   * A template names the object `example` and labels it `app: example`; the
   * Service in the same bundle selects on that label. Renaming only
   * `metadata.name` leaves a Deployment no Service can find — a broken result
   * that looks correct. Only values that still equal the old name are touched,
   * so a label the user set deliberately is never rewritten.
   */
  const rename = (next: string) =>
    setDraft((current) => {
      const previous = String(getPath(current, "metadata.name") ?? "");
      const renamed = setPath(current, "metadata.name", next) as Obj;
      return previous ? (followLabels(renamed, previous, next) as Obj) : renamed;
    });

  const run = async (apply: boolean) => {
    if (!cluster) return;
    setBusy(true);
    setError(null);
    try {
      const object = currentDraft();
      const yaml = stringify(object);
      const ns = descriptor.namespaced ? namespace || null : null;
      if (apply) {
        setResults(await api.applyManifest(cluster, yaml, ns, false));
        setPlan(null);
      } else {
        setPlan(await api.planManifest(cluster, yaml, ns, false));
        setResults(null);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const blocked = plan?.docs.some((doc) => doc.action === "error") ?? false;
  const done = results?.every((entry) => entry.status !== "error" && entry.status !== "conflict");

  return (
    <div className="modal" role="dialog" aria-label={`Create ${descriptor.kind}`}>
      <div className="modal__card modal__card--wide manifest">
        <h3>
          Create {descriptor.kind}
          <button className="dx-prompt__link create__change" onClick={onChangeKind}>
            change kind
          </button>
        </h3>

        <div className="manifest__toolbar">
          <div className="segmented" role="tablist">
            <button
              role="tab"
              aria-selected={mode === "form"}
              className={`segmented__button${mode === "form" ? " segmented__button--active" : ""}`}
              onClick={() => switchMode("form")}
              disabled={!sections}
              title={sections ? undefined : `No form layout for ${descriptor.kind} yet`}
            >
              Form
            </button>
            <button
              role="tab"
              aria-selected={mode === "yaml"}
              className={`segmented__button${mode === "yaml" ? " segmented__button--active" : ""}`}
              onClick={() => switchMode("yaml")}
            >
              YAML
            </button>
          </div>

          {mode === "form" && (
            <label className="manifest__ns">
              Name
              <input
                id="create-name"
                value={String(getPath(draft, "metadata.name") ?? "")}
                onChange={(e) => rename(e.target.value)}
                placeholder="example"
              />
            </label>
          )}

          {descriptor.namespaced && (
            <label className="manifest__ns">
              Namespace
              <input
                list="create-namespaces"
                value={namespace}
                onChange={(e) => setNamespace(e.target.value)}
                placeholder="default"
              />
              <datalist id="create-namespaces">
                {namespaces.map((entry) => (
                  <option key={entry} value={entry} />
                ))}
              </datalist>
            </label>
          )}
          {!descriptor.namespaced && <span className="muted">cluster-scoped</span>}
        </div>

        {mode === "form" && sections && (
          <p className="muted manifest__hint">
            The name identifies the object for good — renaming later means creating a new one and
            deleting this.
          </p>
        )}

        {mode === "form" && sections ? (
          <FormContext.Provider
            value={{ cluster, namespace: descriptor.namespaced ? namespace || null : null, draft }}
          >
            <div className="create__form">
              <Wizard sections={sections} draft={draft} onChange={update} />
            </div>
          </FormContext.Provider>
        ) : (
          <div className="manifest__editor" ref={host} />
        )}

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
                  ? "Fix what the plan reports first"
                  : undefined
            }
          >
            Create
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Replace `previous` with `next` inside label and selector maps only.
 *
 * Scoped deliberately: a blind search-and-replace over the whole document
 * would also rewrite an image tag or a mount path that happened to share the
 * name.
 */
function followLabels(value: unknown, previous: string, next: string, key = ""): unknown {
  if (Array.isArray(value)) {
    return value.map((entry) => followLabels(entry, previous, next, key));
  }
  if (value && typeof value === "object") {
    const inLabels = key === "labels" || key === "matchLabels";
    const out: Obj = {};
    for (const [name, entry] of Object.entries(value as Obj)) {
      out[name] =
        inLabels && entry === previous ? next : followLabels(entry, previous, next, name);
    }
    return out;
  }
  return value;
}

/** Choose what to create, when the page in view does not say. */
function KindPicker({
  discovery,
  suggested,
  onPick,
  onClose,
}: {
  discovery: { groups: { resources: ResourceDescriptor[] }[] } | null;
  suggested: ResourceDescriptor | null;
  onPick: (descriptor: ResourceDescriptor) => void;
  onClose: () => void;
}) {
  const [filter, setFilter] = useState("");

  const creatable = useMemo(() => {
    const all = (discovery?.groups ?? []).flatMap((group) => group.resources);
    const needle = filter.trim().toLowerCase();
    return all
      .filter((entry) => entry.verbs.includes("create"))
      .filter(
        (entry) =>
          !needle ||
          entry.kind.toLowerCase().includes(needle) ||
          entry.plural.toLowerCase().includes(needle),
      )
      .sort((a, b) => a.kind.localeCompare(b.kind))
      .slice(0, 200);
  }, [discovery, filter]);

  return (
    <div className="modal" role="dialog" aria-label="Choose what to create">
      <div className="modal__card">
        <h3>Create</h3>
        <p className="muted">
          {suggested
            ? `This account cannot create ${suggested.kind} here. Choose something else:`
            : "Choose what to create. Opening this from a resource list picks that kind for you."}
        </p>

        <input
          autoFocus
          placeholder="Filter kinds"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />

        <ul className="create__kinds">
          {creatable.map((entry) => (
            <li key={entry.key}>
              <button onClick={() => onPick(entry)}>
                <span className="create__kind">{entry.kind}</span>
                <span className="muted">{entry.apiVersion}</span>
              </button>
            </li>
          ))}
          {creatable.length === 0 && <li className="muted">Nothing matches.</li>}
        </ul>

        <div className="modal__actions">
          <button className="button" onClick={onClose}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
