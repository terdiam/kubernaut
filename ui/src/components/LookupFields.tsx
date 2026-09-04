import { useState } from "react";
import { dependencyValue, useFormScope, useLookup } from "../formContext";
import type { LookupOption } from "../types";

type Obj = Record<string, unknown>;

/**
 * Fields whose values name other objects in the cluster.
 *
 * Typing these by hand is where forms go wrong quietly: a misspelled pull
 * secret, an Ingress backend pointing at a Service that does not exist, a port
 * the Service never exposed. All of them apply cleanly and then do not work.
 * Offering what exists turns those into choices instead of guesses.
 */

/** Shared select, with the current value kept even when it is not on the list. */
function OptionSelect({
  value,
  options,
  loading,
  error,
  allowCustom,
  placeholder,
  onChange,
  id,
}: {
  value: string;
  options: LookupOption[];
  loading: boolean;
  error: string | null;
  allowCustom?: boolean;
  placeholder?: string;
  onChange: (next: string) => void;
  id?: string;
}) {
  // Typing stays available for something not created yet — and it is the only
  // way forward if the cluster cannot be reached for the list.
  const [typing, setTyping] = useState(false);
  const unknown = value !== "" && !options.some((option) => option.value === value);
  const asText = typing || (allowCustom && unknown && !loading) || error !== null;

  if (asText) {
    return (
      <div className="lookup">
        <input
          id={id}
          value={value}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          list={options.length > 0 ? `${id}-options` : undefined}
        />
        {options.length > 0 && (
          <datalist id={`${id}-options`}>
            {options.map((option) => (
              <option key={option.value} value={option.value} label={option.detail ?? undefined} />
            ))}
          </datalist>
        )}
        {error && <span className="muted lookup__note">could not list: {error}</span>}
        {!error && (
          <button type="button" className="dx-prompt__link" onClick={() => setTyping(false)}>
            choose from the cluster
          </button>
        )}
      </div>
    );
  }

  return (
    <div className="lookup">
      <select id={id} value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">{loading ? "loading…" : placeholder ?? "— none —"}</option>
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
            {option.detail ? ` — ${option.detail}` : ""}
          </option>
        ))}
      </select>
      {allowCustom && (
        <button type="button" className="dx-prompt__link" onClick={() => setTyping(true)}>
          type a name
        </button>
      )}
      {!loading && options.length === 0 && !allowCustom && (
        <span className="muted lookup__note">nothing to choose from in this namespace</span>
      )}
    </div>
  );
}

/** A single reference: service account, storage class, ingress class… */
export function LookupField({
  id,
  source,
  dependsOn,
  allowCustom,
  placeholder,
  value,
  onChange,
}: {
  id: string;
  source: string;
  dependsOn?: string;
  allowCustom?: boolean;
  placeholder?: string;
  value: unknown;
  onChange: (next: string | undefined) => void;
}) {
  const { draft } = useFormScope();
  const param = dependencyValue(draft, dependsOn);
  const { options, loading, error } = useLookup(source, param);

  return (
    <OptionSelect
      id={id}
      value={typeof value === "string" ? value : ""}
      options={options}
      loading={loading}
      error={error}
      allowCustom={allowCustom}
      placeholder={placeholder}
      onChange={(next) => onChange(next === "" ? undefined : next)}
    />
  );
}

/** A list of `{ name }` references, as `imagePullSecrets` is shaped. */
export function RefListField({
  source,
  value,
  onChange,
}: {
  source: string;
  value: unknown;
  onChange: (next: Obj[] | undefined) => void;
}) {
  const entries = Array.isArray(value) ? (value as Obj[]) : [];
  const { options, loading, error } = useLookup(source, null);

  const replace = (index: number, name: string) => {
    const next = entries.slice();
    if (name === "") next.splice(index, 1);
    else next[index] = { name };
    onChange(next.length ? next : undefined);
  };

  const unused = options.filter(
    (option) => !entries.some((entry) => entry.name === option.value),
  );

  return (
    <div className="reflist">
      {entries.map((entry, index) => (
        <div key={index} className="reflist__row">
          <OptionSelect
            value={typeof entry.name === "string" ? entry.name : ""}
            options={options}
            loading={loading}
            error={error}
            allowCustom
            onChange={(name) => replace(index, name)}
          />
          <button
            type="button"
            className="icon-button"
            onClick={() => replace(index, "")}
            aria-label="Remove"
          >
            ✕
          </button>
        </div>
      ))}

      <button
        type="button"
        className="button button--ghost"
        onClick={() => onChange([...entries, { name: unused[0]?.value ?? "" }])}
      >
        Add
      </button>

      {!loading && options.length === 0 && !error && (
        <p className="muted lookup__note">
          No registry Secret in this namespace. One has to exist here before a pull can use it —
          a pull secret is namespaced.
        </p>
      )}
    </div>
  );
}

type VolumeKind = "pvc" | "configMap" | "secret";

/**
 * The object-reference key each kind stores its reference under, and inside
 * that, the field holding the referenced name — asymmetric in the
 * Kubernetes API itself (`configMap.name` but `secret.secretName`), not a
 * choice made here.
 */
const VOLUME_KIND: Record<
  VolumeKind,
  { key: string; field: string; source: string; label: string }
> = {
  pvc: { key: "persistentVolumeClaim", field: "claimName", source: "persistentVolumeClaims", label: "Claim" },
  configMap: { key: "configMap", field: "name", source: "configMaps", label: "ConfigMap" },
  secret: { key: "secret", field: "secretName", source: "secrets", label: "Secret" },
};

function volumeKind(volume: Obj): VolumeKind | null {
  if (volume.persistentVolumeClaim !== undefined) return "pvc";
  if (volume.configMap !== undefined) return "configMap";
  if (volume.secret !== undefined) return "secret";
  return null;
}

/**
 * Pod volumes backed by another object: a claim, a ConfigMap or a Secret —
 * the three shapes worth choosing from the cluster rather than typing.
 * Volumes of any other shape (`emptyDir`, `hostPath`, …) are listed read-only
 * so the form never silently drops them.
 */
export function VolumesField({
  value,
  onChange,
}: {
  value: unknown;
  onChange: (next: Obj[] | undefined) => void;
}) {
  const all = Array.isArray(value) ? (value as Obj[]) : [];
  const managed = all.filter((volume) => volumeKind(volume) !== null);
  const others = all.filter((volume) => volumeKind(volume) === null);

  const write = (nextManaged: Obj[]) => {
    const next = [...others, ...nextManaged];
    onChange(next.length ? next : undefined);
  };
  const update = (index: number, patch: Obj) => {
    const next = managed.slice();
    next[index] = { ...next[index], ...patch };
    write(next);
  };
  const setKind = (index: number, kind: VolumeKind) => {
    const { key, field } = VOLUME_KIND[kind];
    // A whole replacement, not a merge — `update` layers its patch onto the
    // existing volume, which would leave the old reference key sitting next
    // to the new one. A volume has exactly one source; an object naming two
    // is rejected by the apiserver.
    const next = managed.slice();
    next[index] = { name: managed[index]!.name, [key]: { [field]: "" } };
    write(next);
  };

  return (
    <div className="volumes">
      {managed.map((volume, index) => {
        const kind = volumeKind(volume)!;
        const { key, field, source } = VOLUME_KIND[kind];
        const refValue = String((volume[key] as Obj | undefined)?.[field] ?? "");
        return (
          <div key={index} className="volumes__row">
            <input
              value={typeof volume.name === "string" ? volume.name : ""}
              placeholder="volume name"
              onChange={(e) => update(index, { name: e.target.value })}
            />
            <select value={kind} onChange={(e) => setKind(index, e.target.value as VolumeKind)}>
              {(Object.keys(VOLUME_KIND) as VolumeKind[]).map((k) => (
                <option key={k} value={k}>
                  {VOLUME_KIND[k].label}
                </option>
              ))}
            </select>
            <LookupField
              id={`volume-${index}-ref`}
              source={source}
              allowCustom
              placeholder={VOLUME_KIND[kind].label.toLowerCase()}
              value={refValue}
              onChange={(next) => update(index, { [key]: { [field]: next ?? "" } })}
            />
            <button
              type="button"
              className="icon-button"
              onClick={() => write(managed.filter((_, at) => at !== index))}
              aria-label="Remove"
            >
              ✕
            </button>
          </div>
        );
      })}

      <button
        type="button"
        className="button button--ghost"
        onClick={() =>
          write([...managed, { name: "data", persistentVolumeClaim: { claimName: "" } }])
        }
      >
        Add volume
      </button>

      {others.length > 0 && (
        <p className="muted lookup__note">
          {others.length} other volume(s) — {others.map((v) => String(v.name)).join(", ")} — are
          kept as they are; edit them in the YAML tab.
        </p>
      )}
    </div>
  );
}
