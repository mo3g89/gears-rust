import { describe, expect, it } from 'vitest';

import {
  ABSENT,
  attrText,
  detailRows,
  FieldDesc,
  isSecretKind,
  pluginForProduct,
  ProductPlugin,
  tableColumns,
} from '@/lib/fieldDesc';

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

/** VHP's own observed schema, in its declaration order. */
const VHP_SCHEMA: FieldDesc[] = [
  field({ key: 'platformVersion', label: 'Version', role: 'version', in_table: true, in_detail: true }),
  field({ key: 'build', label: 'Build', role: 'build', in_detail: true }),
  field({ key: 'namespace', label: 'Namespace', role: 'namespace', in_table: true, in_detail: true }),
  field({ key: 'baseDomain', label: 'Base URL', kind: 'url', role: 'base_url', in_table: true, in_detail: true }),
];

describe('tableColumns', () => {
  it('returns one column per in_table descriptor, in declaration order', () => {
    expect(tableColumns(VHP_SCHEMA).map((f) => f.key)).toEqual([
      'platformVersion',
      'namespace',
      'baseDomain',
    ]);
  });

  it('omits a descriptor that is not marked in_table', () => {
    expect(tableColumns(VHP_SCHEMA).map((f) => f.key)).not.toContain('build');
  });

  it('is empty for a plugin that declares no table columns, rather than throwing', () => {
    expect(tableColumns([])).toEqual([]);
    expect(tableColumns([field({ key: 'internal' })])).toEqual([]);
  });

  it('never returns a secret-kind descriptor, even one flagged in_table', () => {
    const schema = [field({ key: 'kubeconfig', kind: 'multiline_secret', in_table: true })];
    expect(tableColumns(schema)).toEqual([]);
  });
});

describe('detailRows', () => {
  it('returns one row per in_detail descriptor, in declaration order', () => {
    expect(detailRows(VHP_SCHEMA).map((f) => f.key)).toEqual([
      'platformVersion',
      'build',
      'namespace',
      'baseDomain',
    ]);
  });

  it('never returns a secret-kind descriptor, even one flagged in_detail', () => {
    const schema = [field({ key: 'token', kind: 'secret', in_detail: true })];
    expect(detailRows(schema)).toEqual([]);
  });
});

describe('attrText', () => {
  const attrs = { platformVersion: '26.5', namespace: 'virtuozzo' };

  it('renders the observed value', () => {
    expect(attrText(field({ key: 'platformVersion' }), attrs)).toBe('26.5');
  });

  it('renders the absent marker, never "undefined", for an attribute no observation produced', () => {
    const rendered = attrText(field({ key: 'baseDomain' }), attrs);
    expect(rendered).toBe(ABSENT);
    expect(rendered).not.toContain('undefined');
  });

  it('treats an empty string as absent: a blank cell says nothing an operator can act on', () => {
    expect(attrText(field({ key: 'namespace' }), { namespace: '' })).toBe(ABSENT);
  });

  it('renders the absent marker when the environment has never been observed at all', () => {
    expect(attrText(field({ key: 'platformVersion' }), null)).toBe(ABSENT);
    expect(attrText(field({ key: 'platformVersion' }), undefined)).toBe(ABSENT);
  });

  it('NEVER renders a secret value, even when one somehow arrives', () => {
    const leaked = { kubeconfig: 'apiVersion: v1\nBEGIN PRIVATE KEY' };
    const rendered = attrText(
      field({ key: 'kubeconfig', kind: 'multiline_secret', in_table: true }),
      leaked,
    );
    expect(rendered).toBe(ABSENT);
    expect(rendered).not.toContain('PRIVATE KEY');
  });
});

describe('isSecretKind', () => {
  it('covers both credential-bearing kinds and nothing else', () => {
    expect(isSecretKind('secret')).toBe(true);
    expect(isSecretKind('multiline_secret')).toBe(true);
    for (const kind of ['text', 'url', 'int', 'bool', 'enum'] as const) {
      expect(isSecretKind(kind)).toBe(false);
    }
  });
});

describe('pluginForProduct', () => {
  const plugins: ProductPlugin[] = [
    { instance_id: 'gts.a~vhp.v1', vendor: 'virtuozzo-vhp', credential_schema: [], observed_schema: VHP_SCHEMA },
    { instance_id: 'gts.a~other.v1', vendor: 'other', credential_schema: [], observed_schema: [] },
  ];

  it('resolves an instance id to its plugin', () => {
    expect(pluginForProduct(plugins, 'gts.a~vhp.v1')?.vendor).toBe('virtuozzo-vhp');
  });

  it('returns null for an id this deployment does not register, rather than throwing', () => {
    expect(pluginForProduct(plugins, 'gts.a~absent.v1')).toBeNull();
  });

  it('returns null when the product names no plugin', () => {
    expect(pluginForProduct(plugins, null)).toBeNull();
    expect(pluginForProduct(plugins, undefined)).toBeNull();
  });
});
