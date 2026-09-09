// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest';

import { migratePrefixedKeys, readMigrated } from '@/lib/storageKeys';

beforeEach(() => localStorage.clear());

describe('readMigrated', () => {
  it('returns the new value when one is already stored', () => {
    localStorage.setItem('qa.selectedProduct', 'new');
    localStorage.setItem('vhp.selectedProduct', 'old');
    expect(readMigrated('qa.selectedProduct', 'vhp.selectedProduct')).toBe('new');
  });

  it('copies the legacy value across on first read, so nothing is lost to the rename', () => {
    localStorage.setItem('vhp.selectedProduct', 'product-vhp');

    expect(readMigrated('qa.selectedProduct', 'vhp.selectedProduct')).toBe('product-vhp');
    expect(localStorage.getItem('qa.selectedProduct')).toBe('product-vhp');
  });

  it('removes the legacy key once copied, so the migration runs once', () => {
    localStorage.setItem('vhp.selectedBranch', 'release-9.0');
    readMigrated('qa.selectedBranch', 'vhp.selectedBranch');

    expect(localStorage.getItem('vhp.selectedBranch')).toBeNull();
    // And a second read is served from the new key alone.
    expect(readMigrated('qa.selectedBranch', 'vhp.selectedBranch')).toBe('release-9.0');
  });

  it('returns null when neither key holds anything', () => {
    expect(readMigrated('qa.selectedProduct', 'vhp.selectedProduct')).toBeNull();
  });

  it('does not let a new value be clobbered by a stale legacy one', () => {
    localStorage.setItem('qa-theme', 'dark');
    localStorage.setItem('vhp-theme', 'light');

    expect(readMigrated('qa-theme', 'vhp-theme')).toBe('dark');
    expect(localStorage.getItem('qa-theme')).toBe('dark');
  });
});

describe('migratePrefixedKeys', () => {
  it('moves every saved filter across, not just the first', () => {
    localStorage.setItem('vhp:fql:saved:runs', 'state = failed');
    localStorage.setItem('vhp:fql:saved:tests', 'name ~ smoke');
    localStorage.setItem('vhp:fql:saved:plans', 'branch = main');

    migratePrefixedKeys('qa:fql:saved:', 'vhp:fql:saved:');

    expect(localStorage.getItem('qa:fql:saved:runs')).toBe('state = failed');
    expect(localStorage.getItem('qa:fql:saved:tests')).toBe('name ~ smoke');
    expect(localStorage.getItem('qa:fql:saved:plans')).toBe('branch = main');
  });

  it('removes every legacy key, so the migration is idempotent', () => {
    localStorage.setItem('vhp:fql:saved:runs', 'state = failed');
    migratePrefixedKeys('qa:fql:saved:', 'vhp:fql:saved:');

    expect(localStorage.getItem('vhp:fql:saved:runs')).toBeNull();
    migratePrefixedKeys('qa:fql:saved:', 'vhp:fql:saved:');
    expect(localStorage.getItem('qa:fql:saved:runs')).toBe('state = failed');
  });

  it('never overwrites a filter the user has already saved under the new name', () => {
    localStorage.setItem('qa:fql:saved:runs', 'the one I want');
    localStorage.setItem('vhp:fql:saved:runs', 'the one I replaced');

    migratePrefixedKeys('qa:fql:saved:', 'vhp:fql:saved:');

    expect(localStorage.getItem('qa:fql:saved:runs')).toBe('the one I want');
    expect(localStorage.getItem('vhp:fql:saved:runs')).toBeNull();
  });

  it('leaves unrelated keys alone', () => {
    localStorage.setItem('something.else', 'keep me');
    migratePrefixedKeys('qa:fql:saved:', 'vhp:fql:saved:');
    expect(localStorage.getItem('something.else')).toBe('keep me');
  });
});
