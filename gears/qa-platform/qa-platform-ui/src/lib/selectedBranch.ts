import { useSyncExternalStore } from 'react';
import { readMigrated } from '@/lib/storageKeys';

// A single app-wide "currently selected branch", persisted to localStorage and
// shared across the listing pages and the Run/Schedule dialogs so the chosen
// branch is remembered everywhere. Empty string means "repository default".
const STORAGE_KEY = 'qa.selectedBranch';
/** The pre-Task-22 name. Read through once so nobody's branch resets. */
const LEGACY_STORAGE_KEY = 'vhp.selectedBranch';

function readInitial(): string {
  try {
    return readMigrated(STORAGE_KEY, LEGACY_STORAGE_KEY) ?? '';
  } catch {
    return '';
  }
}

let current = readInitial();
const listeners = new Set<() => void>();

export function getSelectedBranch(): string {
  return current;
}

export function setSelectedBranch(branch: string): void {
  const next = branch ?? '';
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

/** Shared, persisted branch selection. Same API shape as useState. */
export function useSelectedBranch(): [string, (branch: string) => void] {
  const value = useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => current,
    () => current
  );
  return [value, setSelectedBranch];
}
