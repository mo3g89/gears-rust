/**
 * The environment credential form, generated from a plugin's
 * `credential_schema()`.
 *
 * The dialog used to render one hardcoded "Kubeconfig" textarea — VHP's single
 * credential, built into a form every product shares. **VHP's form is visually
 * unchanged afterwards**, because its plugin declares `kubeconfig` as
 * `MultilineSecret` and that renders the same textarea.
 *
 * # The request shape, and why it is a map
 *
 * Values are collected into `credentials`, keyed by `FieldDesc.key`, each
 * externally tagged `{"material": "…"}` for a pasted value. That is the shape
 * Task 18b Step 2 defined, and this is its first UI caller — until now the
 * dialog sent the pre-plugin `kubeconfig` pair, which Task 18b kept accepted
 * precisely so this component could be the change that stops sending it.
 *
 * A `{"reference": "…"}` value is the other arm: a credstore reference the
 * operator already holds. The dialog does not offer it yet — nothing in the
 * shipped UI ever did — and the map shape means adding it later changes this
 * component and nothing else.
 */
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import type { FieldDesc } from '@/lib/fieldDesc';

/** One credential value the operator has typed, keyed by `FieldDesc.key`. */
export type CredentialValues = Record<string, string>;

interface CredentialFieldsProps {
  /** The product plugin's `credential_schema()`. Empty renders nothing. */
  schema: FieldDesc[];
  values: CredentialValues;
  onChange: (values: CredentialValues) => void;
}

/**
 * Every required field carries a value.
 *
 * The gear enforces this itself (`require_declared_secrets`, Critical C-2) —
 * this is the form telling the operator before the round trip, not the rule.
 */
export function missingRequired(schema: FieldDesc[], values: CredentialValues): string[] {
  return schema
    .filter((field) => field.required && !(values[field.key] ?? '').trim())
    .map((field) => field.label);
}

/**
 * The `credentials` map for the request.
 *
 * Blank fields are omitted rather than sent as `""`: an empty optional
 * credential is one the operator did not supply, and sending it would have the
 * gear mint a secret out of nothing.
 */
export function credentialsPayload(
  schema: FieldDesc[],
  values: CredentialValues,
): Record<string, { material: string }> {
  const payload: Record<string, { material: string }> = {};
  for (const field of schema) {
    const value = (values[field.key] ?? '').trim();
    if (value) payload[field.key] = { material: value };
  }
  return payload;
}

export function CredentialFields({ schema, values, onChange }: CredentialFieldsProps) {
  if (schema.length === 0) return null;

  return (
    <>
      {schema.map((field) => {
        const id = `credential-${field.key}`;
        const value = values[field.key] ?? '';
        const set = (next: string) => onChange({ ...values, [field.key]: next });

        return (
          <div key={field.key} className="space-y-2">
            <Label htmlFor={id}>
              {field.label}
              {field.required && <span aria-hidden> *</span>}
            </Label>
            {field.kind === 'multiline_secret' ? (
              <Textarea
                id={id}
                value={value}
                onChange={(e) => set(e.target.value)}
                placeholder={`Paste your ${field.label.toLowerCase()} here...`}
                className="font-mono text-sm min-h-[200px]"
                required={field.required}
              />
            ) : (
              <Input
                id={id}
                // `password` for a single-line secret so a shoulder-surfer sees
                // nothing; `text` for everything else, because masking a
                // namespace helps nobody and hurts typo-spotting.
                type={field.kind === 'secret' ? 'password' : 'text'}
                value={value}
                onChange={(e) => set(e.target.value)}
                required={field.required}
              />
            )}
            {field.help && <p className="text-xs text-muted-foreground">{field.help}</p>}
          </div>
        );
      })}
    </>
  );
}
