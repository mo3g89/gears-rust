import { describe, expect, it } from 'vitest';

import { EnvironmentInfo } from '@/api/types';
import { defaultEnvironmentForProduct, defaultEnvironmentLabel } from '@/lib/defaultEnvironment';

const VHP = 'product-vhp';
const OTHER = 'product-other';

function environment(name: string, productId: string | null, isDefault = false): EnvironmentInfo {
  return {
    id: `id-${name}`,
    name,
    created_at: '2026-08-31T00:00:00Z',
    description: '',
    product_id: productId,
    is_default: isDefault,
    available: true,
    observed_attrs: {},
    health_state: 'unknown',
    health_detail: null,
    version: null,
    build: null,
    version_detected_at: null,
    version_detect_error: null,
    default_branch: null,
  } as EnvironmentInfo;
}

describe('defaultEnvironmentForProduct', () => {
  it('resolves to the product\'s environment when it has exactly one', () => {
    const resolved = defaultEnvironmentForProduct([environment('sv-test', VHP)], VHP);
    expect(resolved?.name).toBe('sv-test');
  });

  it('ignores environments belonging to a different product', () => {
    // The bug this rules out is the one that matters most: running a product's suite
    // against another product's environment because the list was not filtered.
    const resolved = defaultEnvironmentForProduct(
      [environment('sv-test', VHP), environment('other-1', OTHER), environment('other-2', OTHER)],
      VHP
    );
    expect(resolved?.name).toBe('sv-test');
  });

  it('refuses to guess when the product has several environments and none is flagged', () => {
    const resolved = defaultEnvironmentForProduct(
      [environment('sv-test', VHP), environment('sv-stage', VHP)],
      VHP
    );
    expect(resolved).toBeNull();
  });

  it('uses the flagged environment when the product has several', () => {
    // The whole point of the flag: disambiguate what "exactly one" cannot.
    const resolved = defaultEnvironmentForProduct(
      [environment('sv-test', VHP), environment('sv-stage', VHP, true)],
      VHP
    );
    expect(resolved?.name).toBe('sv-stage');
  });

  it('picks the flagged environment out of a product that also owns unflagged ones', () => {
    // This does NOT pin the order of the two tiers, and an earlier version of this
    // comment claimed it did. Break-testing showed the claim was false: checking
    // "exactly one" first and checking the flag first are equivalent functions. They
    // can only disagree when a product owns exactly one environment, and there both
    // return that environment. The order in the implementation is therefore a readability
    // choice, not a behavioural one — do not add a test asserting it, because none can
    // fail.
    const resolved = defaultEnvironmentForProduct(
      [environment('sv-test', VHP), environment('sv-stage', VHP, true), environment('other', OTHER)],
      VHP
    );
    expect(resolved?.name).toBe('sv-stage');
  });

  it('never honours a flag set on another product\'s environment', () => {
    const resolved = defaultEnvironmentForProduct(
      [environment('sv-a', VHP), environment('sv-b', VHP), environment('other', OTHER, true)],
      VHP
    );
    expect(resolved).toBeNull();
  });

  it('returns null when the product has no environment at all', () => {
    expect(defaultEnvironmentForProduct([environment('other', OTHER)], VHP)).toBeNull();
  });

  it('returns null for an unknown product rather than picking a global environment', () => {
    // A plan with no product must not silently inherit someone else's environment.
    expect(defaultEnvironmentForProduct([environment('sv-test', VHP)], null)).toBeNull();
    expect(defaultEnvironmentForProduct([environment('sv-test', VHP)], undefined)).toBeNull();
  });

  it('never treats a product-less environment as a candidate', () => {
    expect(defaultEnvironmentForProduct([environment('orphan', null)], VHP)).toBeNull();
  });
});

describe('defaultEnvironmentLabel', () => {
  it('names the environment it will actually run against', () => {
    expect(defaultEnvironmentLabel(environment('sv-test', VHP))).toBe('Default cluster (sv-test)');
  });

  it('promises nothing when the default cannot be resolved', () => {
    expect(defaultEnvironmentLabel(null)).toBe('Default cluster');
  });
});
