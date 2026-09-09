/**
 * Renaming a `localStorage` key without losing what is behind it.
 *
 * Every key this app owned was named `vhp.*`, `vhp-*` or `vhp:*` — the product
 * name baked into the browser of every operator who has ever used it. Renaming
 * them to `qa.*` is the last of the VHP coupling, and a rename alone would
 * silently reset everybody's selected product, branch, theme and **saved
 * filters**: values a user typed, not values the server can send again.
 *
 * So the read is a one-time read-through: on a miss under the new key, look
 * under the old one, copy it across, and remove the old. Idempotent, so it
 * costs one extra lookup once and nothing thereafter.
 */

/** Read `key`, migrating a value stored under `legacyKey` on first access. */
export function readMigrated(key: string, legacyKey: string): string | null {
  try {
    const current = localStorage.getItem(key);
    if (current !== null) return current;

    const legacy = localStorage.getItem(legacyKey);
    if (legacy === null) return null;

    localStorage.setItem(key, legacy);
    localStorage.removeItem(legacyKey);
    return legacy;
  } catch {
    // A browser with storage disabled must not take the page down with it.
    // Every caller already treats `null` as "nothing remembered".
    return null;
  }
}

/**
 * Migrate every key under `legacyPrefix` to `prefix`, once.
 *
 * For the saved-filter keys, where the suffix is user-chosen and the set is not
 * known ahead of time. An existing key under the new prefix always wins — a
 * user who has already saved a filter under the new name must not have it
 * overwritten by a stale one from before the rename.
 */
export function migratePrefixedKeys(prefix: string, legacyPrefix: string): void {
  try {
    const legacyKeys: string[] = [];
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (key && key.startsWith(legacyPrefix)) legacyKeys.push(key);
    }
    // Collected first, then mutated: removing while iterating by index skips
    // entries, which would leave half the filters behind.
    for (const legacyKey of legacyKeys) {
      const suffix = legacyKey.slice(legacyPrefix.length);
      const value = localStorage.getItem(legacyKey);
      if (value !== null && localStorage.getItem(`${prefix}${suffix}`) === null) {
        localStorage.setItem(`${prefix}${suffix}`, value);
      }
      localStorage.removeItem(legacyKey);
    }
  } catch {
    // As above: storage being unavailable is not a reason to fail a render.
  }
}
