// The settings pages' credential-reference placeholders are references the
// gears accept. The Slack form used to suggest `cred://slack-webhook`, which
// qa-environments and qa-catalog refused and qa-insights now refuses too.
import { describe, expect, it } from 'vitest';
import { CREDSTORE_REF_PATTERN, CREDSTORE_REF_PLACEHOLDERS } from './credstoreRef';

describe('credential-store reference placeholders', () => {
  it.each(Object.entries(CREDSTORE_REF_PLACEHOLDERS))('%s is a valid reference', (_, value) => {
    expect(value).toMatch(CREDSTORE_REF_PATTERN);
  });

  it('the pattern refuses the spellings the gears refuse', () => {
    for (const bad of ['cred://slack-webhook', 'credstore://a/b', 'a:b', '', 'x'.repeat(256)]) {
      expect(bad).not.toMatch(CREDSTORE_REF_PATTERN);
    }
    expect('x'.repeat(255)).toMatch(CREDSTORE_REF_PATTERN);
  });
});
