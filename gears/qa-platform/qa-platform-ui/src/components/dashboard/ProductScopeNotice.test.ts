// @vitest-environment jsdom
//
// Bug: `DashboardPage` mounts only inside `RequireProduct`, which blocks
// rendering until a product is selected -- so a version of this notice keyed
// on "is a product selected" would always be `true` there and render on
// every visit, permanently. Reviewed and fixed to key on
// `DashboardStats.unattributable_runs` instead, which the gear computes fresh
// per request and which is `0` whenever nothing was actually dropped. This
// suite is the regression guard: it asserts absence at `0` and presence
// (with the count and correct pluralization) above it.
import { createElement } from 'react';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { ProductScopeNotice } from './ProductScopeNotice';

afterEach(cleanup);

describe('ProductScopeNotice', () => {
  it('renders nothing when no run was dropped as unattributable', () => {
    const { container } = render(createElement(ProductScopeNotice, { unattributableRuns: 0 }));
    expect(container.firstChild).toBeNull();
  });

  it('renders the count, singular, for exactly one dropped run', () => {
    render(createElement(ProductScopeNotice, { unattributableRuns: 1 }));
    expect(screen.queryByText(/1 custom plan run counted under no product/)).not.toBeNull();
  });

  it('renders the count, plural, for more than one dropped run', () => {
    render(createElement(ProductScopeNotice, { unattributableRuns: 3 }));
    expect(screen.queryByText(/3 custom plan runs counted under no product/)).not.toBeNull();
  });

  // I5 (final review): the copy used to read "Custom plans span more than one
  // repository, so they cannot be attributed to the selected product" -- a
  // general claim about custom plans that the Runs list one click away
  // disproves, since `productScope.ts` does attribute them through the
  // repositories their tests name. The notice must say which side cannot do it.
  it('attributes the limitation to this gear rather than to custom plans in general', () => {
    render(createElement(ProductScopeNotice, { unattributableRuns: 2 }));
    expect(screen.queryByText(/This gear can't tell which product a custom plan belongs to/)).not.toBeNull();
    expect(screen.queryByText(/cannot be attributed to the selected product/)).toBeNull();
  });

  // The count is every unattributable custom-plan run in the page the dashboard
  // read, not "runs of your product that were dropped" -- there is no product to
  // compare them against. The copy must not claim otherwise.
  it('says the runs are counted under no product rather than excluded from this one', () => {
    render(createElement(ProductScopeNotice, { unattributableRuns: 2 }));
    expect(screen.queryByText(/not under this one and not under any other/)).not.toBeNull();
  });
});
