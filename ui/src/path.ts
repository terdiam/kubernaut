/**
 * Dotted-path access for form fields.
 *
 * Kubernetes keys legitimately contain dots (`kubernetes.io/hostname`), so a
 * naive split corrupts them. `\.` escapes a literal dot in a path segment.
 */

export function splitPath(path: string): string[] {
  const parts: string[] = [];
  let current = "";
  for (let i = 0; i < path.length; i += 1) {
    const char = path[i];
    if (char === "\\" && path[i + 1] === ".") {
      current += ".";
      i += 1;
    } else if (char === ".") {
      parts.push(current);
      current = "";
    } else {
      current += char;
    }
  }
  parts.push(current);
  return parts.filter((p) => p.length > 0);
}

export function getPath(root: unknown, path: string): unknown {
  let node: unknown = root;
  for (const key of splitPath(path)) {
    if (node == null || typeof node !== "object") return undefined;
    node = (node as Record<string, unknown>)[key];
  }
  return node;
}

/**
 * Immutably set a value, creating intermediate objects. Setting `undefined`,
 * `null` or `""` deletes the key — an empty form field must not become an
 * empty string in the manifest, which is a different thing to the apiserver.
 */
export function setPath<T>(root: T, path: string, value: unknown): T {
  const keys = splitPath(path);
  if (keys.length === 0) return root;

  const clone = structuredClone(root) as Record<string, unknown>;
  let node = clone;

  for (let i = 0; i < keys.length - 1; i += 1) {
    const key = keys[i]!;
    const next = node[key];
    if (next == null || typeof next !== "object" || Array.isArray(next)) {
      node[key] = {};
    }
    node = node[key] as Record<string, unknown>;
  }

  const last = keys[keys.length - 1]!;
  if (value === undefined || value === null || value === "") {
    delete node[last];
  } else {
    node[last] = value;
  }
  return clone as T;
}
