// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';

import { credentialsPayload, missingRequired } from './CredentialFields';
import type { FieldDesc } from '@/lib/fieldDesc';

function field(overrides: Partial<FieldDesc> & Pick<FieldDesc, 'key'>): FieldDesc {
  return {
    label: overrides.key,
    kind: 'text',
    required: false,
    role: null,
    in_table: false,
    in_detail: false,
    help: null,
    ...overrides,
  };
}

/** VHP's own credential schema: one required multiline secret, one text field. */
const VHP_SCHEMA: FieldDesc[] = [
  field({ key: 'kubeconfig', label: 'Kubeconfig', kind: 'multiline_secret', required: true }),
  field({ key: 'vpadm_namespace', label: 'vpadm namespace' }),
];

describe('credentialsPayload', () => {
  it('builds the externally tagged map Task 18b Step 2 defined', () => {
    // Trimmed, as the dialog has always trimmed a pasted kubeconfig: trailing
    // whitespace is not part of a YAML document, and storing it would make two
    // identical pastes mint two different secrets.
    expect(credentialsPayload(VHP_SCHEMA, { kubeconfig: 'apiVersion: v1\n' })).toEqual({
      kubeconfig: { material: 'apiVersion: v1' },
    });
  });

  it('omits a blank field rather than sending an empty string', () => {
    // Sending `""` would have the gear mint a secret out of nothing.
    const payload = credentialsPayload(VHP_SCHEMA, {
      kubeconfig: 'apiVersion: v1\n',
      vpadm_namespace: '   ',
    });
    expect(payload).not.toHaveProperty('vpadm_namespace');
  });

  it('carries every supplied field, not just the first', () => {
    const payload = credentialsPayload(VHP_SCHEMA, {
      kubeconfig: 'apiVersion: v1\n',
      vpadm_namespace: 'virtuozzo',
    });
    expect(Object.keys(payload).sort()).toEqual(['kubeconfig', 'vpadm_namespace']);
  });

  it('sends nothing a plugin does not declare, even if the form somehow holds it', () => {
    const payload = credentialsPayload(VHP_SCHEMA, { kubeconfig: 'x', stray: 'value' });
    expect(payload).not.toHaveProperty('stray');
  });

  it('trims, so a pasted value with trailing whitespace is stored as typed', () => {
    expect(credentialsPayload(VHP_SCHEMA, { kubeconfig: '  y  ' })).toEqual({
      kubeconfig: { material: 'y' },
    });
  });
});

describe('missingRequired', () => {
  it('names a required field the operator left blank', () => {
    expect(missingRequired(VHP_SCHEMA, {})).toEqual(['Kubeconfig']);
  });

  it('is empty once every required field has a value', () => {
    expect(missingRequired(VHP_SCHEMA, { kubeconfig: 'apiVersion: v1\n' })).toEqual([]);
  });

  it('does not demand an optional field', () => {
    expect(missingRequired(VHP_SCHEMA, { kubeconfig: 'x' })).not.toContain('vpadm namespace');
  });

  it('treats whitespace as blank', () => {
    expect(missingRequired(VHP_SCHEMA, { kubeconfig: '   ' })).toEqual(['Kubeconfig']);
  });
});
