/**
 * Secrets round-trip through the form as decoded text.
 *
 * The manifest stores base64 in `data`. Writing `stringData` instead would let
 * the apiserver encode for us, but `stringData` merges rather than replaces, so
 * a key removed in the form would silently survive. Decoding into the form and
 * re-encoding into `data` keeps the key set exactly as the user left it.
 */

type Obj = Record<string, unknown>;

function decodeBase64(value: string): string | null {
  try {
    const binary = atob(value);
    const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    // Binary secrets (certificates, keystores) are not text; they stay in the
    // manifest untouched and are edited in the YAML tab.
    return null;
  }
}

function encodeBase64(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let binary = "";
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte);
  });
  return btoa(binary);
}

/** Live Secret → form draft, with decodable values moved to `stringData`. */
export function secretToForm(object: Obj): Obj {
  const data = (object.data ?? {}) as Record<string, string>;
  const decoded: Record<string, string> = {};
  const binary: Record<string, string> = {};

  for (const [key, value] of Object.entries(data)) {
    const text = decodeBase64(value);
    if (text === null) binary[key] = value;
    else decoded[key] = text;
  }

  const draft: Obj = { ...object, stringData: decoded };
  if (Object.keys(binary).length > 0) draft.data = binary;
  else delete draft.data;
  return draft;
}

/** Form draft → manifest, re-encoding everything back into `data`. */
export function secretFromForm(draft: Obj): Obj {
  const decoded = (draft.stringData ?? {}) as Record<string, string>;
  const binary = (draft.data ?? {}) as Record<string, string>;

  const data: Record<string, string> = { ...binary };
  for (const [key, value] of Object.entries(decoded)) {
    data[key] = encodeBase64(value);
  }

  const object: Obj = { ...draft, data };
  delete object.stringData;
  return object;
}

/** True when a Secret holds values this editor cannot show as text. */
export function hasBinaryData(object: Obj): boolean {
  const data = (object.data ?? {}) as Record<string, string>;
  return Object.values(data).some((value) => decodeBase64(value) === null);
}
