/**
 * The keys of the actions in flight. Each action adds and removes only its
 * own key, so one that finishes leaves the others busy.
 */
export type BusyKeys = ReadonlySet<string>;

export const NO_BUSY_KEYS: BusyKeys = new Set();

export function withKey(keys: BusyKeys, key: string): BusyKeys {
  return keys.has(key) ? keys : new Set(keys).add(key);
}

export function withoutKey(keys: BusyKeys, key: string): BusyKeys {
  if (!keys.has(key)) return keys;
  const next = new Set(keys);
  next.delete(key);
  return next;
}
