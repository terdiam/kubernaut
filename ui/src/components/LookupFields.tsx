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

/**
 * Pod volumes backed by a claim.
 *
 * Only the claim-backed shape is modelled: it is the one that references
 * another object and therefore the one worth choosing rather than typing.
 * Volumes of any other shape are listed read-only so the form never silently
 * drops them.
 */
export function VolumesField({
  value,
  onChange,
}: {
  value: unknown;
  onChange: (next: Obj[] | undefined) => void;
}) {
  const all = Array.isArray(value) ? (value as Obj[]) : [];
  const claims = all.filter((volume) => volume.persistentVolumeClaim !== undefined);
  const others = all.filter((volume) => volume.persistentVolumeClaim === undefined);
  const { options, loading, error } = useLookup("persistentVolumeClaims", null);

  const write = (nextClaims: Obj[]) => {
    const next = [...others, ...nextClaims];
    onChange(next.length ? next : undefined);
  };
  const update = (index: number, patch: Obj) => {
    const next = claims.slice();
    next[index] = { ...next[index], ...patch };
    write(next);
  };

  return (
    <div className="volumes">
      {claims.map((volume, index) => (
        <div key={index} className="volumes__row">
          <input
            value={typeof volume.name === "string" ? volume.name : ""}
            placeholder="volume name"
            onChange={(e) => update(index, { name: e.target.value })}
          />
          <OptionSelect
            value={String(
              (volume.persistentVolumeClaim as Obj | undefined)?.claimName ?? "",
            )}
            options={options}
            loading={loading}
            error={error}
            allowCustom
            placeholder="claim"
            onChange={(claimName) => update(index, { persistentVolumeClaim: { claimName } })}
          />
          <button
            type="button"
            className="icon-button"
            onClick={() => write(claims.filter((_, at) => at !== index))}
            aria-label="Remove"
          >
            ✕
          </button>
        </div>
      ))}

      <button
        type="button"
        className="button button--ghost"
        onClick={() => write([...claims, { name: "data", persistentVolumeClaim: { claimName: "" } }])}
      >
        Add claim volume
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
