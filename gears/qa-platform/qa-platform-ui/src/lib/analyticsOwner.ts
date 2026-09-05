const STORAGE_KEY = 'analytics_owner_id';

function createOwnerId(): string {
  const randomPart = `${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
  return `owner-${randomPart}`;
}

export function getAnalyticsOwnerId(): string {
  if (typeof window === 'undefined') {
    return 'owner-server';
  }

  const existing = window.localStorage.getItem(STORAGE_KEY)?.trim();
  if (existing) {
    return existing;
  }

  const generated = createOwnerId();
  window.localStorage.setItem(STORAGE_KEY, generated);
  return generated;
}
