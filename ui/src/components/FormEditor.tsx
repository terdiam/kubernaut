import { useMemo, useState } from "react";
import { api } from "../api";
import { formSections, type Field, type Section } from "../formSpec";
import { getPath, setPath } from "../path";
import { prunedApply } from "../applyPrune";
import { FormContext, useFormScope } from "../formContext";
import { LookupField, RefListField, VolumesField } from "./LookupFields";
import { IngressRulesField, IngressTlsField } from "./IngressRules";
import type { DiffResult, FieldConflict } from "../types";

interface Props {
  cluster: string;
  resource: string;
  group: string;
  kind: string;
  namespace: string | null;
  name: string;
  /** Live object as JSON, already in form shape. */
  initial: Record<string, unknown>;
  /** Convert the draft back to a manifest (Secrets re-encode here). */
  serialize?: (draft: Record<string, unknown>) => unknown;
  onApplied: () => void;
}

type Obj = Record<string, unknown>;

/**
 * Structured editing, Rancher-style: the fields that matter for a kind, grouped
 * and explained, over the same server-side apply path the YAML tab uses. Fields
 * absent from a layout are preserved untouched — the form edits the live object
 * rather than rebuilding it.
 */
export function FormEditor(props: Props) {
  const { cluster, resource, group, kind, namespace, name, initial, serialize, onApplied } =
    props;
  const [draft, setDraft] = useState<Obj>(initial);
  const [diff, setDiff] = useState<DiffResult | null>(null);
  const [conflicts, setConflicts] = useState<FieldConflict[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const sections = useMemo(() => formSections(group, kind), [group, kind]);
  const dirty = useMemo(
    () => JSON.stringify(draft) !== JSON.stringify(initial),
    [draft, initial],
  );

  if (!sections) {
    return (
      <p className="muted form__empty">
        No form layout for {kind} yet — use the YAML tab, which is schema-validated
        against this cluster.
      </p>
    );
  }

  const update = (path: string, value: unknown) =>
    setDraft((current) => setPath(current, path, value));

  // Apply only what changed. Sending the whole object would claim every field
  // in it, so an edit to one field conflicted with whoever owns the rest.
  // JSON is valid YAML, so the same apply path serves both editors.
  const request = (force: boolean) => {
    const live = (serialize ? serialize(initial) : initial) as Obj;
    const next = (serialize ? serialize(draft) : draft) as Obj;
    return {
      resource,
      namespace,
      name,
      yaml: JSON.stringify(prunedApply(live, next) ?? next, null, 2),
      force,
    };
  };

  const preview = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await api.previewEdit(cluster, request(false));
      setDiff(result);
      setConflicts(result.conflicts.length > 0 ? result.conflicts : null);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const save = async (force: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const outcome = await api.applyEdit(cluster, request(force));
      if (outcome.status === "conflict") {
        setConflicts(outcome.conflicts);
        return;
      }
      setDiff(null);
      setConflicts(null);
      onApplied();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="form">
      <div className="form__toolbar">
        <button className="button" onClick={() => void preview()} disabled={!dirty || busy}>
          Preview changes
        </button>
        <button
          className="button button--primary"
          onClick={() => void save(false)}
          disabled={!dirty || busy}
        >
          Save
        </button>
        <button className="button" onClick={() => setDraft(initial)} disabled={!dirty}>
          Reset
        </button>
        {dirty && <span className="muted">unsaved changes</span>}
        {error && <span className="error">{error}</span>}
      </div>

      {conflicts && (
        <ConflictPanel
          conflicts={conflicts}
          busy={busy}
          onForce={() => void save(true)}
          onDismiss={() => setConflicts(null)}
        />
      )}

      <FormContext.Provider value={{ cluster, namespace, draft }}>
      <div className="form__body">
        {sections.map((section: Section) => (
          <fieldset key={section.title} className="form__section">
            <legend>{section.title}</legend>
            {section.description && <p className="muted form__note">{section.description}</p>}
            {section.fields.map((field) => (
              <FieldRow key={field.path} field={field} value={getPath(draft, field.path)} onChange={update} />
            ))}
          </fieldset>
        ))}
      </div>
      </FormContext.Provider>

      {diff && (
        <div className="diff">
          <header className="diff__head">
            <strong>{diff.changed ? "Proposed changes" : "No changes"}</strong>
            <div className="diff__actions">
              {diff.changed && (
                <button className="button button--primary" onClick={() => void save(false)}>
                  Apply
                </button>
              )}
              <button className="icon-button" onClick={() => setDiff(null)}>
                ✕
              </button>
            </div>
          </header>
          {diff.unified && <pre className="diff__body">{diff.unified}</pre>}
        </div>
      )}
    </div>
  );
}

/**
 * A refused apply, and the one control that resolves it.
 *
 * Forcing takes the field from its owner; if that owner is a controller it will
 * set it back on its next sync, so the owner is named rather than hidden behind
 * a generic retry.
 */
function ConflictPanel({
  conflicts,
  busy,
  onForce,
  onDismiss,
}: {
  conflicts: FieldConflict[];
  busy: boolean;
  onForce: () => void;
  onDismiss: () => void;
}) {
  const managers = [...new Set(conflicts.map((c) => c.manager))];
  return (
    <div className="conflict">
      <header className="conflict__head">
        <strong>Owned by {managers.join(", ")}</strong>
        <button className="icon-button" onClick={onDismiss} title="Dismiss">
          ✕
        </button>
      </header>
      <ul className="conflict__fields">
        {conflicts.map((conflict) => (
          <li key={`${conflict.manager}-${conflict.field}`}>
            <code>{conflict.field || "(field not reported)"}</code>
            <span className="muted"> — {conflict.manager}</span>
          </li>
        ))}
      </ul>
      <p className="muted conflict__note">
        Saving takes {conflicts.length === 1 ? "this field" : "these fields"} from{" "}
        {managers.join(", ")}. A controller will set{" "}
        {conflicts.length === 1 ? "it" : "them"} back on its next sync.
      </p>
      <button className="button button--danger" onClick={onForce} disabled={busy}>
        Take ownership and save
      </button>
    </div>
  );
}

export function FieldRow({
  field,
  value,
  onChange,
}: {
  field: Field;
  value: unknown;
  onChange: (path: string, value: unknown) => void;
}) {
  // Tie the label to its control, so clicking the label focuses the input and
  // a screen reader announces the two together.
  const id = `field-${field.path}`;
  const label = (
    <label className="field__label" htmlFor={id} title={field.help}>
      {field.label}
      {field.help && <span className="field__help">{field.help}</span>}
    </label>
  );

  switch (field.kind) {
    case "text":
      return (
        <div className="field">
          {label}
          <input
            id={id}
            value={typeof value === "string" ? value : ""}
            placeholder={field.placeholder}
            onChange={(e) => onChange(field.path, e.target.value)}
          />
        </div>
      );

    case "number":
      return (
        <div className="field">
          {label}
          <input
            id={id}
            type="number"
            min={field.min}
            value={typeof value === "number" ? value : ""}
            onChange={(e) =>
              onChange(field.path, e.target.value === "" ? undefined : Number(e.target.value))
            }
          />
        </div>
      );

    case "boolean":
      return (
        <div className="field field--inline">
          <input
            id={id}
            type="checkbox"
            checked={value === true}
            onChange={(e) => onChange(field.path, e.target.checked ? true : undefined)}
          />
          {label}
        </div>
      );

    case "select":
      return (
        <div className="field">
          {label}
          <select
            id={id}
            value={typeof value === "string" ? value : ""}
            onChange={(e) => onChange(field.path, e.target.value || undefined)}
          >
            <option value="">(unset)</option>
            {field.options.map((option) => (
              <option key={option} value={option}>
                {option}
              </option>
            ))}
          </select>
        </div>
      );

    case "textarea":
      return (
        <div className="field field--wide">
          {label}
          <JsonArea value={value} onChange={(next) => onChange(field.path, next)} />
        </div>
      );

    case "keyValue":
      return (
        <div className="field field--wide">
          {label}
          <KeyValueEditor
            value={(value ?? {}) as Record<string, string>}
            masked={field.masked === true}
            onChange={(next) => onChange(field.path, Object.keys(next).length ? next : undefined)}
          />
        </div>
      );

    case "stringList":
      return (
        <div className="field field--wide">
          {label}
          <StringListEditor
            value={Array.isArray(value) ? (value as string[]) : []}
            onChange={(next) => onChange(field.path, next.length ? next : undefined)}
          />
        </div>
      );

    case "containers":
      return (
        <ContainersEditor
          value={Array.isArray(value) ? (value as Obj[]) : []}
          // The pod's own volumes live one level up, at a path this field's own
          // path always predicts (`<prefix>.spec.containers` next to
          // `<prefix>.spec.volumes` — see `podTemplate`) — so a mount can offer
          // what the Volumes field already declared, without either field
          // needing to know about the other's existence.
          volumesPath={field.path.replace(/\.containers$/, ".volumes")}
          onChange={(next) => onChange(field.path, next)}
        />
      );

    case "servicePorts":
      return (
        <div className="field field--wide">
          {label}
          <ServicePortsEditor
            value={Array.isArray(value) ? (value as Obj[]) : []}
            onChange={(next) => onChange(field.path, next.length ? next : undefined)}
          />
        </div>
      );

    case "lookup":
      return (
        <div className="field">
          {label}
          <LookupField
            id={id}
            source={field.source}
            dependsOn={field.dependsOn}
            allowCustom={field.allowCustom}
            placeholder={field.placeholder}
            value={value}
            onChange={(next) => onChange(field.path, next)}
          />
        </div>
      );

    case "refList":
      return (
        <div className="field field--wide">
          {label}
          <RefListField
            source={field.source}
            value={value}
            onChange={(next) => onChange(field.path, next)}
          />
        </div>
      );

    case "volumes":
      return (
        <div className="field field--wide">
          {label}
          <VolumesField value={value} onChange={(next) => onChange(field.path, next)} />
        </div>
      );

    case "ingressRules":
      return (
        <div className="field field--wide">
          {label}
          <IngressRulesField value={value} onChange={(next) => onChange(field.path, next)} />
        </div>
      );

    case "ingressTls":
      return (
        <div className="field field--wide">
          {label}
          <IngressTlsField value={value} onChange={(next) => onChange(field.path, next)} />
        </div>
      );

    case "volumeClaimTemplates":
      return (
        <div className="field field--wide">
          {label}
          <VolumeClaimTemplatesEditor
            value={Array.isArray(value) ? (value as Obj[]) : []}
            onChange={(next) => onChange(field.path, next.length ? next : undefined)}
          />
        </div>
      );
  }
}

/** Nested structures the form does not model get a JSON box, not a dead end. */
function JsonArea({ value, onChange }: { value: unknown; onChange: (next: unknown) => void }) {
  const [text, setText] = useState(() =>
    value === undefined ? "" : JSON.stringify(value, null, 2),
  );
  const [error, setError] = useState<string | null>(null);

  return (
    <>
      <textarea
        rows={6}
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          if (e.target.value.trim() === "") {
            setError(null);
            onChange(undefined);
            return;
          }
          try {
            onChange(JSON.parse(e.target.value));
            setError(null);
          } catch (err) {
            setError(String(err));
          }
        }}
      />
      {error && <span className="error field__error">{error}</span>}
    </>
  );
}

function KeyValueEditor({
  value,
  masked = false,
  onChange,
}: {
  value: Record<string, string>;
  masked?: boolean;
  onChange: (next: Record<string, string>) => void;
}) {
  const entries = Object.entries(value);
  // Secret values stay hidden until asked for, so a shared screen does not leak
  // them just because the tab was open.
  const [revealed, setRevealed] = useState<Set<string>>(new Set());
  const toggle = (key: string) =>
    setRevealed((current) => {
      const next = new Set(current);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  const replace = (index: number, key: string, val: string) => {
    const next = entries.slice();
    next[index] = [key, val];
    onChange(Object.fromEntries(next.filter(([k]) => k !== "")));
  };

  return (
    <div className="kv">
      {entries.map(([key, val], index) => (
        <div className="kv__row" key={`${key}-${index}`}>
          <input value={key} onChange={(e) => replace(index, e.target.value, val)} />
          <input
            value={String(val)}
            type={masked && !revealed.has(key) ? "password" : "text"}
            onChange={(e) => replace(index, key, e.target.value)}
          />
          {masked && (
            <button
              className="icon-button"
              title={revealed.has(key) ? "Hide value" : "Reveal value"}
              onClick={() => toggle(key)}
            >
              {revealed.has(key) ? "🙈" : "👁"}
            </button>
          )}
          <button
            className="icon-button"
            title="Remove"
            onClick={() => onChange(Object.fromEntries(entries.filter((_, i) => i !== index)))}
          >
            ✕
          </button>
        </div>
      ))}
      <button className="button button--ghost" onClick={() => onChange({ ...value, "": "" })}>
        + Add
      </button>
    </div>
  );
}

function StringListEditor({
  value,
  onChange,
}: {
  value: string[];
  onChange: (next: string[]) => void;
}) {
  return (
    <div className="kv">
      {value.map((item, index) => (
        <div className="kv__row" key={index}>
          <input
            value={item}
            onChange={(e) => {
              const next = value.slice();
              next[index] = e.target.value;
              onChange(next);
            }}
          />
          <button
            className="icon-button"
            onClick={() => onChange(value.filter((_, i) => i !== index))}
          >
            ✕
          </button>
        </div>
      ))}
      <button className="button button--ghost" onClick={() => onChange([...value, ""])}>
        + Add
      </button>
    </div>
  );
}

function ServicePortsEditor({
  value,
  onChange,
}: {
  value: Obj[];
  onChange: (next: Obj[]) => void;
}) {
  const patch = (index: number, key: string, val: unknown) => {
    const next = value.slice();
    const entry = { ...(next[index] ?? {}) };
    if (val === "" || val === undefined) delete entry[key];
    else entry[key] = val;
    next[index] = entry;
    onChange(next);
  };

  return (
    <div className="ports">
      <div className="ports__head">
        <span>Name</span>
        <span>Port</span>
        <span>Target</span>
        <span>Protocol</span>
        <span />
      </div>
      {value.map((port, index) => (
        <div className="ports__row" key={index}>
          <input
            value={String(port.name ?? "")}
            onChange={(e) => patch(index, "name", e.target.value)}
          />
          <input
            type="number"
            value={String(port.port ?? "")}
            onChange={(e) => patch(index, "port", Number(e.target.value))}
          />
          <input
            value={String(port.targetPort ?? "")}
            onChange={(e) => {
              const raw = e.target.value;
              const numeric = Number(raw);
              patch(index, "targetPort", raw === "" ? "" : Number.isNaN(numeric) ? raw : numeric);
            }}
          />
          <select
            value={String(port.protocol ?? "TCP")}
            onChange={(e) => patch(index, "protocol", e.target.value)}
          >
            <option>TCP</option>
            <option>UDP</option>
            <option>SCTP</option>
          </select>
          <button
            className="icon-button"
            onClick={() => onChange(value.filter((_, i) => i !== index))}
          >
            ✕
          </button>
        </div>
      ))}
      <button
        className="button button--ghost"
        onClick={() => onChange([...value, { port: 80, protocol: "TCP" }])}
      >
        + Add port
      </button>
    </div>
  );
}

function ContainersEditor({
  value,
  volumesPath,
  onChange,
}: {
  value: Obj[];
  volumesPath: string;
  onChange: (next: Obj[]) => void;
}) {
  const { draft } = useFormScope();
  // Names only, and only the ones the Volumes field manages (claim-backed) —
  // the rest are still real volumes, just not ones this form declared, so
  // typing their name into a mount is still allowed, just not suggested.
  const podVolumes = getPath(draft, volumesPath);
  // `volumeClaimTemplates` is always at this one path, unlike the pod template
  // above it — StatefulSet is the only kind with it, and Kubernetes gives each
  // template an implicit volume of the same name, with no `spec.volumes` entry
  // needed. A different kind simply has nothing at this path.
  const claimTemplates = getPath(draft, "spec.volumeClaimTemplates");
  const volumeNames = [
    ...(Array.isArray(podVolumes) ? podVolumes : []),
    ...(Array.isArray(claimTemplates) ? claimTemplates : []),
  ]
    .map((v) => {
      if (!v || typeof v !== "object") return "";
      const direct = (v as Obj).name;
      if (typeof direct === "string") return direct;
      const meta = (v as Obj).metadata;
      return meta && typeof meta === "object" ? String((meta as Obj).name ?? "") : "";
    })
    .filter(Boolean);

  const patch = (index: number, key: string, val: unknown) => {
    const next = value.slice();
    const entry = { ...(next[index] ?? {}) };
    if (val === undefined || val === "") delete entry[key];
    else entry[key] = val;
    next[index] = entry;
    onChange(next);
  };

  const patchResource = (index: number, bucket: string, key: string, val: string) => {
    const next = value.slice();
    const entry = { ...(next[index] ?? {}) };
    const resources = { ...((entry.resources ?? {}) as Obj) };
    const group = { ...((resources[bucket] ?? {}) as Obj) };
    if (val === "") delete group[key];
    else group[key] = val;
    if (Object.keys(group).length === 0) delete resources[bucket];
    else resources[bucket] = group;
    if (Object.keys(resources).length === 0) delete entry.resources;
    else entry.resources = resources;
    next[index] = entry;
    onChange(next);
  };

  // Only the two key-backed shapes are modelled — the ones that name a
  // ConfigMap or Secret and therefore benefit from a picker. `fieldRef` and
  // `resourceFieldRef` (downward API) are left exactly as the existing
  // disabled-input fallback already handles them.
  const envSource = (variable: Obj): "literal" | "configMap" | "secret" | "other" => {
    if (variable.valueFrom === undefined) return "literal";
    const from = variable.valueFrom as Obj;
    if (from.configMapKeyRef !== undefined) return "configMap";
    if (from.secretKeyRef !== undefined) return "secret";
    return "other";
  };
  const envRef = (variable: Obj, source: "configMap" | "secret"): { name: string; key: string } => {
    const refKey = source === "configMap" ? "configMapKeyRef" : "secretKeyRef";
    const ref = ((variable.valueFrom as Obj)?.[refKey] as Obj) ?? {};
    return { name: String(ref.name ?? ""), key: String(ref.key ?? "") };
  };
  const setEnvSource = (
    index: number,
    env: Obj[],
    position: number,
    source: "literal" | "configMap" | "secret",
  ) => {
    const name = env[position]!.name;
    const next = env.slice();
    next[position] =
      source === "literal"
        ? { name, value: "" }
        : {
            name,
            valueFrom: {
              [source === "configMap" ? "configMapKeyRef" : "secretKeyRef"]: { name: "", key: "" },
            },
          };
    patch(index, "env", next);
  };
  const setEnvRef = (
    index: number,
    env: Obj[],
    position: number,
    source: "configMap" | "secret",
    patchRef: Partial<{ name: string; key: string }>,
  ) => {
    const refKey = source === "configMap" ? "configMapKeyRef" : "secretKeyRef";
    const next = env.slice();
    next[position] = {
      ...next[position],
      valueFrom: { [refKey]: { ...envRef(next[position]!, source), ...patchRef } },
    };
    patch(index, "env", next);
  };

  // `envFrom` imports every key of a ConfigMap or Secret as an environment
  // variable at once — the bulk counterpart to picking one key at a time.
  const envFromKind = (entry: Obj): "configMap" | "secret" | null => {
    if (entry.configMapRef !== undefined) return "configMap";
    if (entry.secretRef !== undefined) return "secret";
    return null;
  };
  const setEnvFrom = (index: number, envFrom: Obj[], position: number, next: Obj) => {
    const all = envFrom.slice();
    all[position] = next;
    patch(index, "envFrom", all);
  };

  const resourceValue = (container: Obj, bucket: string, key: string) => {
    const resources = (container.resources ?? {}) as Obj;
    const group = (resources[bucket] ?? {}) as Obj;
    return String(group[key] ?? "");
  };

  return (
    <div className="containers">
      {value.map((container, index) => {
        const env = Array.isArray(container.env) ? (container.env as Obj[]) : [];
        const envFrom = Array.isArray(container.envFrom) ? (container.envFrom as Obj[]) : [];
        const mounts = Array.isArray(container.volumeMounts) ? (container.volumeMounts as Obj[]) : [];
        return (
          <div className="containers__item" key={index}>
            <div className="containers__head">
              <input
                className="containers__name"
                value={String(container.name ?? "")}
                placeholder="name"
                onChange={(e) => patch(index, "name", e.target.value)}
              />
              <button
                className="icon-button"
                title="Remove container"
                onClick={() => onChange(value.filter((_, i) => i !== index))}
              >
                ✕
              </button>
            </div>

            <div className="field">
              <label className="field__label">Image</label>
              <input
                value={String(container.image ?? "")}
                onChange={(e) => patch(index, "image", e.target.value)}
              />
            </div>

            <div className="field">
              <label className="field__label">
                Image pull policy
                <span className="field__help">
                  `Always` re-pulls on every start; `IfNotPresent` is the usual choice.
                </span>
              </label>
              <select
                value={String(container.imagePullPolicy ?? "")}
                onChange={(e) => patch(index, "imagePullPolicy", e.target.value)}
              >
                <option value="">(default)</option>
                <option>Always</option>
                <option>IfNotPresent</option>
                <option>Never</option>
              </select>
            </div>

            <div className="field__grid">
              {(["requests", "limits"] as const).map((bucket) =>
                (["cpu", "memory"] as const).map((key) => (
                  <div className="field" key={`${bucket}-${key}`}>
                    <label className="field__label">
                      {bucket} {key}
                    </label>
                    <input
                      value={resourceValue(container, bucket, key)}
                      placeholder={key === "cpu" ? "500m" : "512Mi"}
                      onChange={(e) => patchResource(index, bucket, key, e.target.value)}
                    />
                  </div>
                )),
              )}
            </div>

            <div className="field field--wide">
              <label className="field__label">Environment</label>
              <div className="kv">
                {env.map((variable, position) => {
                  const source = envSource(variable);
                  return (
                    <div className="kv__row" key={position}>
                      <input
                        value={String(variable.name ?? "")}
                        placeholder="NAME"
                        onChange={(e) => {
                          const next = env.slice();
                          next[position] = { ...variable, name: e.target.value };
                          patch(index, "env", next);
                        }}
                      />
                      {source === "other" ? (
                        <input value="" placeholder="(from reference — edit in YAML)" disabled />
                      ) : (
                        <select
                          value={source}
                          onChange={(e) =>
                            setEnvSource(
                              index,
                              env,
                              position,
                              e.target.value as "literal" | "configMap" | "secret",
                            )
                          }
                        >
                          <option value="literal">Value</option>
                          <option value="configMap">ConfigMap key</option>
                          <option value="secret">Secret key</option>
                        </select>
                      )}
                      {source === "literal" && (
                        <input
                          value={String(variable.value ?? "")}
                          placeholder="value"
                          onChange={(e) => {
                            const next = env.slice();
                            next[position] = { ...variable, value: e.target.value };
                            patch(index, "env", next);
                          }}
                        />
                      )}
                      {(source === "configMap" || source === "secret") && (
                        <>
                          <LookupField
                            id={`env-${index}-${position}-ref`}
                            source={source === "configMap" ? "configMaps" : "secrets"}
                            allowCustom
                            placeholder={source === "configMap" ? "ConfigMap" : "Secret"}
                            value={envRef(variable, source).name}
                            onChange={(next) => setEnvRef(index, env, position, source, { name: next ?? "" })}
                          />
                          <input
                            value={envRef(variable, source).key}
                            placeholder="key"
                            onChange={(e) =>
                              setEnvRef(index, env, position, source, { key: e.target.value })
                            }
                          />
                        </>
                      )}
                      <button
                        className="icon-button"
                        onClick={() => patch(index, "env", env.filter((_, i) => i !== position))}
                      >
                        ✕
                      </button>
                    </div>
                  );
                })}
                <button
                  className="button button--ghost"
                  onClick={() => patch(index, "env", [...env, { name: "", value: "" }])}
                >
                  + Add variable
                </button>
              </div>
            </div>

            <div className="field field--wide">
              <label className="field__label">
                Environment from
                <span className="field__help">
                  Every key of a ConfigMap or Secret, imported as an environment variable at once.
                </span>
              </label>
              <div className="kv">
                {envFrom.map((entry, position) => {
                  const kind = envFromKind(entry);
                  if (!kind) return null;
                  const refKey = kind === "configMap" ? "configMapRef" : "secretRef";
                  const refName = String(((entry[refKey] as Obj) ?? {}).name ?? "");
                  return (
                    <div className="kv__row" key={position}>
                      <select
                        value={kind}
                        onChange={(e) =>
                          setEnvFrom(index, envFrom, position, {
                            [e.target.value === "configMap" ? "configMapRef" : "secretRef"]: { name: "" },
                          })
                        }
                      >
                        <option value="configMap">ConfigMap</option>
                        <option value="secret">Secret</option>
                      </select>
                      <LookupField
                        id={`envfrom-${index}-${position}`}
                        source={kind === "configMap" ? "configMaps" : "secrets"}
                        allowCustom
                        value={refName}
                        onChange={(next) =>
                          setEnvFrom(index, envFrom, position, { [refKey]: { name: next ?? "" } })
                        }
                      />
                      <input
                        value={String(entry.prefix ?? "")}
                        placeholder="prefix (optional)"
                        onChange={(e) => {
                          const next = { ...entry };
                          if (e.target.value === "") delete next.prefix;
                          else next.prefix = e.target.value;
                          setEnvFrom(index, envFrom, position, next);
                        }}
                      />
                      <button
                        className="icon-button"
                        onClick={() => patch(index, "envFrom", envFrom.filter((_, i) => i !== position))}
                      >
                        ✕
                      </button>
                    </div>
                  );
                })}
                <button
                  className="button button--ghost"
                  onClick={() =>
                    patch(index, "envFrom", [...envFrom, { configMapRef: { name: "" } }])
                  }
                >
                  + Add source
                </button>
              </div>
            </div>

            <div className="field field--wide">
              <label className="field__label">
                Volume mounts
                <span className="field__help">
                  Name matches a volume from Volumes below, or one of this workload's volume
                  claim templates.
                </span>
              </label>
              <div className="kv">
                {mounts.map((mount, position) => (
                  <div className="kv__row" key={position}>
                    <input
                      value={String(mount.name ?? "")}
                      placeholder="which volume"
                      list={volumeNames.length > 0 ? `container-${index}-volumes` : undefined}
                      onChange={(e) => {
                        const next = mounts.slice();
                        next[position] = { ...mount, name: e.target.value };
                        patch(index, "volumeMounts", next);
                      }}
                    />
                    <input
                      value={String(mount.mountPath ?? "")}
                      placeholder="/path/in/container"
                      onChange={(e) => {
                        const next = mounts.slice();
                        next[position] = { ...mount, mountPath: e.target.value };
                        patch(index, "volumeMounts", next);
                      }}
                    />
                    <label className="field--inline" title="Read only">
                      <input
                        type="checkbox"
                        checked={mount.readOnly === true}
                        onChange={(e) => {
                          const next = mounts.slice();
                          const updated = { ...mount };
                          if (e.target.checked) updated.readOnly = true;
                          else delete updated.readOnly;
                          next[position] = updated;
                          patch(index, "volumeMounts", next);
                        }}
                      />
                      RO
                    </label>
                    <button
                      className="icon-button"
                      onClick={() =>
                        patch(index, "volumeMounts", mounts.filter((_, i) => i !== position))
                      }
                    >
                      ✕
                    </button>
                  </div>
                ))}
                {volumeNames.length > 0 && (
                  <datalist id={`container-${index}-volumes`}>
                    {volumeNames.map((name) => (
                      <option key={name} value={name} />
                    ))}
                  </datalist>
                )}
                <button
                  className="button button--ghost"
                  onClick={() =>
                    patch(index, "volumeMounts", [
                      ...mounts,
                      { name: volumeNames[0] ?? "", mountPath: "" },
                    ])
                  }
                >
                  + Add mount
                </button>
              </div>
            </div>
          </div>
        );
      })}
      <button
        className="button button--ghost"
        onClick={() => onChange([...value, { name: "", image: "" }])}
      >
        + Add container
      </button>
    </div>
  );
}

const ACCESS_MODES = ["ReadWriteOnce", "ReadOnlyMany", "ReadWriteMany", "ReadWriteOncePod"];

/**
 * StatefulSet-only: one PVC per replica, provisioned from each template.
 *
 * Distinct from the pod-level Volumes field, which references a claim that
 * already exists — a template instead tells the cluster to create one, and
 * only StatefulSet has this. Access mode is modelled as one choice because
 * that is what every template in practice uses; a multi-mode claim can still
 * be built in the YAML tab.
 */
function VolumeClaimTemplatesEditor({
  value,
  onChange,
}: {
  value: Obj[];
  onChange: (next: Obj[]) => void;
}) {
  const name = (t: Obj) => String(((t.metadata as Obj)?.name as string) ?? "");
  const capacity = (t: Obj) =>
    String((((t.spec as Obj)?.resources as Obj)?.requests as Obj)?.storage ?? "");
  const storageClass = (t: Obj) => String((t.spec as Obj)?.storageClassName ?? "");
  const accessMode = (t: Obj) => {
    const modes = (t.spec as Obj)?.accessModes;
    return Array.isArray(modes) && typeof modes[0] === "string" ? modes[0] : "ReadWriteOnce";
  };

  const patch = (index: number, next: Obj) => {
    const all = value.slice();
    all[index] = next;
    onChange(all);
  };
  const patchName = (index: number, next: string) =>
    patch(index, { ...value[index], metadata: { ...(value[index]!.metadata as Obj), name: next } });
  const patchSpec = (index: number, key: string, val: unknown) => {
    const spec = { ...((value[index]!.spec as Obj) ?? {}) };
    if (val === undefined || val === "") delete spec[key];
    else spec[key] = val;
    patch(index, { ...value[index], spec });
  };
  const patchCapacity = (index: number, storage: string) => {
    const spec = { ...((value[index]!.spec as Obj) ?? {}) };
    const resources = { ...((spec.resources as Obj) ?? {}) };
    const requests = { ...((resources.requests as Obj) ?? {}) };
    if (storage === "") delete requests.storage;
    else requests.storage = storage;
    patch(index, { ...value[index], spec: { ...spec, resources: { ...resources, requests } } });
  };

  return (
    <div className="ports">
      <div className="ports__head">
        <span>Name</span>
        <span>Capacity</span>
        <span>Storage class</span>
        <span>Access mode</span>
        <span />
      </div>
      {value.map((template, index) => (
        <div className="ports__row" key={index}>
          <input value={name(template)} onChange={(e) => patchName(index, e.target.value)} />
          <input
            value={capacity(template)}
            placeholder="10Gi"
            onChange={(e) => patchCapacity(index, e.target.value)}
          />
          <LookupField
            id={`vct-${index}-storageclass`}
            source="storageClasses"
            allowCustom
            placeholder="(default)"
            value={storageClass(template)}
            onChange={(next) => patchSpec(index, "storageClassName", next)}
          />
          <select
            value={accessMode(template)}
            onChange={(e) => patchSpec(index, "accessModes", [e.target.value])}
          >
            {ACCESS_MODES.map((mode) => (
              <option key={mode} value={mode}>
                {mode}
              </option>
            ))}
          </select>
          <button
            className="icon-button"
            onClick={() => onChange(value.filter((_, i) => i !== index))}
          >
            ✕
          </button>
        </div>
      ))}
      <button
        className="button button--ghost"
        onClick={() =>
          onChange([
            ...value,
            {
              metadata: { name: "data" },
              spec: {
                accessModes: ["ReadWriteOnce"],
                resources: { requests: { storage: "1Gi" } },
              },
            },
          ])
        }
      >
        + Add volume claim template
      </button>
    </div>
  );
}
