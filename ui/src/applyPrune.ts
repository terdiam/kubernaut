/**
 * Reduce a form draft to the fields it actually changed.
 *
 * Server-side apply takes ownership of every field in the document it is sent.
 * The form loads the whole live object, so applying it verbatim claimed fields
 * the user never touched — on a Rancher-managed Deployment, editing the replica
 * count conflicted on `.spec.template.spec.containers[name="x"].image`, which
 * `rancher` owns and the user had not edited.
 *
 * Sending only what changed makes the app own only what it set, so a conflict
 * means what it says: the user really is editing a field somebody else owns.
 */

type Obj = Record<string, unknown>;

/** Keys that identify a list entry, by the field name the list appears under. */
const LIST_KEYS: Record<string, string[][]> = {
  containers: [["name"]],
  initContainers: [["name"]],
  ephemeralContainers: [["name"]],
  env: [["name"]],
  volumes: [["name"]],
  volumeMounts: [["mountPath"]],
  imagePullSecrets: [["name"]],
  // Container ports and Service ports share a field name but not their keys.
  ports: [["containerPort", "protocol"], ["port", "protocol"], ["name"]],
};

const isObj = (value: unknown): value is Obj =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** The key set that identifies entries of `list`, if we know one. */
function listKeys(field: string, list: unknown[]): string[] | null {
  for (const candidate of LIST_KEYS[field] ?? []) {
    const known = list.every(
      (entry) => isObj(entry) && candidate.every((key) => entry[key] !== undefined),
    );
    if (known) return candidate;
  }
  return null;
}

const identity = (entry: Obj, keys: string[]) =>
  JSON.stringify(keys.map((key) => entry[key]));

interface Pruned {
  changed: boolean;
  value: unknown;
}

function pruneList(field: string, base: unknown[], next: unknown[]): Pruned {
  const keys = listKeys(field, next);

  // Without keys the apiserver treats the list as one value, and so must we.
  // Reordered, grown or shrunk lists are replaced wholesale too: dropping an
  // entry is expressed by owning the list and leaving the entry out.
  if (!keys || base.length !== next.length) return { changed: true, value: next };

  const byIdentity = new Map(
    base.filter(isObj).map((entry) => [identity(entry, keys), entry]),
  );

  const entries: unknown[] = [];
  for (const entry of next) {
    if (!isObj(entry)) return { changed: true, value: next };
    const previous = byIdentity.get(identity(entry, keys));
    // A new or renamed entry has no counterpart to diff against.
    if (!previous) return { changed: true, value: next };

    const pruned = pruneObject(previous, entry);
    if (!pruned.changed) continue;
    // The keys go with every entry, or the apiserver cannot match it up.
    const value = pruned.value as Obj;
    for (const key of keys) value[key] = entry[key];
    entries.push(value);
  }

  return entries.length === 0
    ? { changed: false, value: undefined }
    : { changed: true, value: entries };
}

function pruneObject(base: Obj, next: Obj): Pruned {
  // Removing a key means owning its parent: apply deletes a field by owning it
  // and leaving it out, so the whole object has to be claimed.
  for (const key of Object.keys(base)) {
    if (!(key in next) && base[key] !== undefined) return { changed: true, value: next };
  }

  const out: Obj = {};
  let changed = false;
  for (const [key, value] of Object.entries(next)) {
    const previous = base[key];
    if (same(previous, value)) continue;

    if (isObj(previous) && isObj(value)) {
      const pruned = pruneObject(previous, value);
      if (pruned.changed) {
        out[key] = pruned.value;
        changed = true;
      }
      continue;
    }

    if (Array.isArray(previous) && Array.isArray(value)) {
      const pruned = pruneList(key, previous, value);
      if (pruned.changed) {
        out[key] = pruned.value;
        changed = true;
      }
      continue;
    }

    out[key] = value;
    changed = true;
  }

  return { changed, value: out };
}

/**
 * The document to apply: identity plus the changed fields, or `null` when
 * nothing changed.
 */
export function prunedApply(live: Obj, draft: Obj): Obj | null {
  const pruned = pruneObject(live, draft);
  if (!pruned.changed) return null;

  const meta = isObj(draft.metadata) ? draft.metadata : {};
  const body = pruned.value as Obj;
  const prunedMeta = isObj(body.metadata) ? body.metadata : {};

  return {
    apiVersion: draft.apiVersion,
    kind: draft.kind,
    ...body,
    // Name and namespace are how the apiserver finds the object, so they are
    // always present even when untouched.
    metadata: {
      ...prunedMeta,
      name: meta.name,
      ...(meta.namespace === undefined ? {} : { namespace: meta.namespace }),
    },
  };
}
