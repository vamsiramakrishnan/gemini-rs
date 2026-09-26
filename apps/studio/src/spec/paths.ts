// Immutable reads and writes by path into plain JSON documents.

export type Path = readonly (string | number)[];
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };

export function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function getIn(doc: unknown, path: Path): Json | undefined {
  let node: unknown = doc;
  for (const key of path) {
    if (Array.isArray(node) && typeof key === 'number') node = node[key];
    else if (isObject(node) && typeof key === 'string') node = node[key];
    else return undefined;
  }
  return node as Json | undefined;
}

/** A copy of `doc` with `value` at `path`, creating containers on the way.
 * Setting `undefined` removes the key (or array element). */
export function setIn<T>(doc: T, path: Path, value: Json | undefined): T {
  if (path.length === 0) return value as T;
  const [key, ...rest] = path;
  if (typeof key === 'number') {
    const array = Array.isArray(doc) ? [...doc] : [];
    if (rest.length === 0 && value === undefined) {
      array.splice(key, 1);
    } else {
      array[key] = setIn(array[key], rest, value);
    }
    return array as T;
  }
  const object: JsonObject = isObject(doc) ? { ...doc } : {};
  if (rest.length === 0 && value === undefined) {
    delete object[key as string];
  } else {
    const next = setIn(object[key as string], rest, value);
    if (next === undefined) delete object[key as string];
    else object[key as string] = next as Json;
  }
  return object as T;
}

/** Drop empty strings, empty arrays and empty objects from optional fields,
 * so the document stays as small as what was authored. */
export function prune(value: Json): Json | undefined {
  if (Array.isArray(value)) {
    return value.map((v) => prune(v) ?? null);
  }
  if (isObject(value)) {
    const out: JsonObject = {};
    for (const [k, v] of Object.entries(value)) {
      const pruned = prune(v);
      if (pruned === undefined) continue;
      if (Array.isArray(pruned) && pruned.length === 0) continue;
      if (isObject(pruned) && Object.keys(pruned).length === 0) continue;
      out[k] = pruned;
    }
    return out;
  }
  if (value === '') return undefined;
  return value;
}
