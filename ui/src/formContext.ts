import { createContext, useContext, useEffect, useState } from "react";
import { api } from "./api";
import { getPath } from "./path";
import type { LookupOption } from "./types";

/**
 * What a form field needs in order to ask the cluster about itself.
 *
 * A reference field ("which Secret", "which Service port") cannot be rendered
 * from the layout alone: it needs the cluster it will be applied to, the
 * namespace it will land in, and the rest of the draft, because one field's
 * options often depend on another's value.
 */
export interface FormScope {
  cluster: string | null;
  namespace: string | null;
  /** The draft being edited, for fields whose options depend on another field. */
  draft: unknown;
}

export const FormContext = createContext<FormScope>({
  cluster: null,
  namespace: null,
  draft: {},
});

export function useFormScope(): FormScope {
  return useContext(FormContext);
}

interface Entry {
  at: number;
  options: Promise<LookupOption[]>;
}

/**
 * Options are cached briefly and shared between fields.
 *
 * A pod template can hold several fields backed by the same list, and a form
 * that re-fetched per field would hit the apiserver once per render. The TTL
 * is short because the point of these lists is to reflect what exists now —
 * a Secret created in another window should show up without a restart.
 */
const CACHE = new Map<string, Entry>();
const TTL_MS = 30_000;

export function invalidateLookups() {
  CACHE.clear();
}

function fetchOptions(
  cluster: string,
  source: string,
  namespace: string | null,
  param: string | null,
  now: number,
): Promise<LookupOption[]> {
  const key = `${cluster}|${source}|${namespace ?? ""}|${param ?? ""}`;
  const hit = CACHE.get(key);
  if (hit && now - hit.at < TTL_MS) return hit.options;

  const options = api.lookupOptions(cluster, source, namespace, param).catch((err) => {
    // A failed lookup must not poison the cache, or the field stays empty for
    // the whole TTL after one blip.
    CACHE.delete(key);
    throw err;
  });
  CACHE.set(key, { at: now, options });
  return options;
}

export interface LookupState {
  options: LookupOption[];
  loading: boolean;
  /** Set when the cluster could not be asked; the field falls back to typing. */
  error: string | null;
}

/**
 * Options for one reference field, refetched when its dependency changes.
 *
 * `enabled` exists for dependent lookups: an Ingress backend port has no
 * meaning until a Service is chosen, and asking anyway costs one failing
 * request per path row on every render.
 */
export function useLookup(source: string, param: string | null, enabled = true): LookupState {
  const { cluster, namespace } = useFormScope();
  const [state, setState] = useState<LookupState>({
    options: [],
    loading: Boolean(cluster) && enabled,
    error: null,
  });

  useEffect(() => {
    if (!cluster || !enabled) {
      setState({ options: [], loading: false, error: null });
      return;
    }
    let cancelled = false;
    setState((current) => ({ ...current, loading: true, error: null }));

    fetchOptions(cluster, source, namespace, param, Date.now())
      .then((options) => !cancelled && setState({ options, loading: false, error: null }))
      .catch(
        (err) => !cancelled && setState({ options: [], loading: false, error: String(err) }),
      );

    return () => {
      cancelled = true;
    };
  }, [cluster, source, namespace, param, enabled]);

  return state;
}

/** Read the value a dependent lookup is parameterised by. */
export function dependencyValue(draft: unknown, path: string | undefined): string | null {
  if (!path) return null;
  const value = getPath(draft, path);
  return typeof value === "string" ? value : value == null ? null : String(value);
}
