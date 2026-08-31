import { describe, expect, it } from 'vitest';

import { PlatformInfo } from '@/api/types';
import { defaultPlatformForProduct, defaultPlatformLabel } from '@/lib/defaultPlatform';

const VHP = 'product-vhp';
const OTHER = 'product-other';

function platform(name: string, productId: string | null, isDefault = false): PlatformInfo {
  return {
    id: `id-${name}`,
    name,
    created_at: '2026-08-31T00:00:00Z',
    description: '',
    product_id: productId,
    is_default: isDefault,
    version: null,
    build: null,
    namespace: null,
    vhp_base_url: null,
    version_detected_at: null,
    version_detect_error: null,
    default_branch: null,
  } as PlatformInfo;
}

describe('defaultPlatformForProduct', () => {
  it('resolves to the product\'s platform when it has exactly one', () => {
    const resolved = defaultPlatformForProduct([platform('sv-test', VHP)], VHP);
    expect(resolved?.name).toBe('sv-test');
  });

  it('ignores platforms belonging to a different product', () => {
    // The bug this rules out is the one that matters most: running a product's suite
    // against another product's environment because the list was not filtered.
    const resolved = defaultPlatformForProduct(
      [platform('sv-test', VHP), platform('other-1', OTHER), platform('other-2', OTHER)],
      VHP
    );
    expect(resolved?.name).toBe('sv-test');
  });

  it('refuses to guess when the product has several platforms and none is flagged', () => {
    const resolved = defaultPlatformForProduct(
      [platform('sv-test', VHP), platform('sv-stage', VHP)],
      VHP
    );
    expect(resolved).toBeNull();
  });

  it('uses the flagged platform when the product has several', () => {
    // The whole point of the flag: disambiguate what "exactly one" cannot.
    const resolved = defaultPlatformForProduct(
      [platform('sv-test', VHP), platform('sv-stage', VHP, true)],
      VHP
    );
    expect(resolved?.name).toBe('sv-stage');
  });

  it('picks the flagged platform out of a product that also owns unflagged ones', () => {
    // This does NOT pin the order of the two tiers, and an earlier version of this
    // comment claimed it did. Break-testing showed the claim was false: checking
    // "exactly one" first and checking the flag first are equivalent functions. They
    // can only disagree when a product owns exactly one platform, and there both
    // return that platform. The order in the implementation is therefore a readability
    // choice, not a behavioural one — do not add a test asserting it, because none can
    // fail.
    const resolved = defaultPlatformForProduct(
      [platform('sv-test', VHP), platform('sv-stage', VHP, true), platform('other', OTHER)],
      VHP
    );
    expect(resolved?.name).toBe('sv-stage');
  });

  it('never honours a flag set on another product\'s platform', () => {
    const resolved = defaultPlatformForProduct(
      [platform('sv-a', VHP), platform('sv-b', VHP), platform('other', OTHER, true)],
      VHP
    );
    expect(resolved).toBeNull();
  });

  it('returns null when the product has no platform at all', () => {
    expect(defaultPlatformForProduct([platform('other', OTHER)], VHP)).toBeNull();
  });

  it('returns null for an unknown product rather than picking a global platform', () => {
    // A plan with no product must not silently inherit someone else's environment.
    expect(defaultPlatformForProduct([platform('sv-test', VHP)], null)).toBeNull();
    expect(defaultPlatformForProduct([platform('sv-test', VHP)], undefined)).toBeNull();
  });

  it('never treats a product-less platform as a candidate', () => {
    expect(defaultPlatformForProduct([platform('orphan', null)], VHP)).toBeNull();
  });
});

describe('defaultPlatformLabel', () => {
  it('names the platform it will actually run against', () => {
    expect(defaultPlatformLabel(platform('sv-test', VHP))).toBe('Default cluster (sv-test)');
  });

  it('promises nothing when the default cannot be resolved', () => {
    expect(defaultPlatformLabel(null)).toBe('Default cluster');
  });
});
