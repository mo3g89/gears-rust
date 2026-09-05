/**
 * Rendering rules for a product plugin's field descriptors.
 *
 * A plugin declares what its observations mean — `observed_schema()` — and this
 * turns that into columns and detail rows. Before Task 21 the environments
 * table hardcoded "Namespace" and "VHP URL", which is the coupling this whole
 * branch exists to remove: those are two of VHP's attributes, not two facts
 * every product has.
 *
 * **VHP's own table looks identical afterwards**, because its plugin declares
 * `baseDomain` with `in_table: true`. That is the copper thread — the rendering
 * moved, the rendered result did not.
 */

/** How a value is entered and displayed. Mirrors the gear's `FieldKindDto`. */
export type FieldKind =
  | 'text'
  | 'url'
  | 'int'
  | 'bool'
  | 'enum'
  | 'secret'
  | 'multiline_secret';

/** What a field *means* to the platform, where the platform needs to know. */
export type FieldRole = 'version' | 'build' | 'base_url' | 'namespace';

/** One field a plugin declares. Mirrors the gear's `FieldDescDto`. */
export interface FieldDesc {
  key: string;
  label: string;
  kind: FieldKind;
  required: boolean;
  role: FieldRole | null;
  in_table: boolean;
  in_detail: boolean;
  help: string | null;
}

/** One registered plugin instance, from `GET /qa/v1/product-plugins`. */
export interface ProductPlugin {
  instance_id: string;
  vendor: string | null;
  credential_schema: FieldDesc[];
  observed_schema: FieldDesc[];
}

/**
 * The two kinds that carry credential material.
 *
 * **A secret descriptor never renders a value**, even if one somehow reaches
 * the browser. The gear is structurally incapable of publishing one —
 * `observed_attrs` is bounded by `retain_declared` and `EnvironmentDto` drops
 * the credential columns outright — so this is the third door on a value that
 * should never have got this far, not the only one. The 2026-08-28 incident is
 * why there are three.
 */
export function isSecretKind(kind: FieldKind): boolean {
  return kind === 'secret' || kind === 'multiline_secret';
}

/** The descriptors that become table columns, in declaration order. */
export function tableColumns(schema: FieldDesc[]): FieldDesc[] {
  return schema.filter((field) => field.in_table && !isSecretKind(field.kind));
}

/** The descriptors that become detail rows, in declaration order. */
export function detailRows(schema: FieldDesc[]): FieldDesc[] {
  return schema.filter((field) => field.in_detail && !isSecretKind(field.kind));
}

/** What a cell with no observed value shows. */
export const ABSENT = '—';

/**
 * The text for one descriptor against one environment's observed attributes.
 *
 * `undefined` never reaches the DOM: an attribute the plugin declares but no
 * observation has produced renders {@link ABSENT}, which reads as "not
 * observed" rather than as a bug.
 */
export function attrText(
  field: FieldDesc,
  observedAttrs: Record<string, string> | null | undefined,
): string {
  if (isSecretKind(field.kind)) return ABSENT;
  const value = observedAttrs?.[field.key];
  if (value === undefined || value === null || value === '') return ABSENT;
  return value;
}

/** The plugin bound to a product, or `null` when nothing resolves it. */
export function pluginForProduct(
  plugins: ProductPlugin[],
  pluginInstanceId: string | null | undefined,
): ProductPlugin | null {
  if (!pluginInstanceId) return null;
  return plugins.find((p) => p.instance_id === pluginInstanceId) ?? null;
}
