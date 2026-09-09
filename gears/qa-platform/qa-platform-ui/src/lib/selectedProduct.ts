import { useSyncExternalStore } from 'react';
import { readMigrated } from '@/lib/storageKeys';

const STORAGE_KEY = 'qa.selectedProduct';
/** The pre-Task-22 name. Read through once so nobody's selection resets. */
const LEGACY_STORAGE_KEY = 'vhp.selectedProduct';

let current = readInitial();
const listeners = new Set<() => void>();

function readInitial(): string {
  try {
    return readMigrated(STORAGE_KEY, LEGACY_STORAGE_KEY) ?? '';
  } catch {
    return '';
  }
}

export function getSelectedProduct(): string {
  return current;
}

export function setSelectedProduct(productId: string): void {
  const next = productId ?? '';
  if (next === current) return;
  current = next;
  try {
    if (next) localStorage.setItem(STORAGE_KEY, next);
    else localStorage.removeItem(STORAGE_KEY);
  } catch {
    // ignore storage failures (private mode, quota)
  }
  listeners.forEach((l) => l());
}

export function useSelectedProduct(): [string, (productId: string) => void] {
  const value = useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => current,
    () => current
  );
  return [value, setSelectedProduct];
}
