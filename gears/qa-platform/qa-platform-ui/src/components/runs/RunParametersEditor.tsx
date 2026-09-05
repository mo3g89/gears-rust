import { Plus, Trash2 } from 'lucide-react';
import type { RunParameter } from '@/api/types';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';

interface RunParametersEditorProps {
  value: RunParameter[];
  onChange: (next: RunParameter[]) => void;
  disabled?: boolean;
}

/** Drop rows with a blank name so a stray empty row doesn't reach the API
 *  (the backend rejects a value with no name). Callers use this to build the
 *  launch payload. */
export function cleanRunParameters(parameters: RunParameter[]): RunParameter[] {
  return parameters
    .map((parameter) => ({ name: parameter.name.trim(), value: parameter.value }))
    .filter((parameter) => parameter.name.length > 0);
}

/**
 * Inline key/value editor for per-launch run parameters. Purely local state
 * owned by the caller (ephemeral, not persisted) — each parameter is injected
 * into the runner as an environment variable for this run only.
 */
export function RunParametersEditor({ value, onChange, disabled }: RunParametersEditorProps) {
  const updateRow = (index: number, patch: Partial<RunParameter>) => {
    onChange(value.map((row, rowIndex) => (rowIndex === index ? { ...row, ...patch } : row)));
  };

  const addRow = () => onChange([...value, { name: '', value: '' }]);

  const removeRow = (index: number) => onChange(value.filter((_, rowIndex) => rowIndex !== index));

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <Label className="text-sm">Parameters (Optional)</Label>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-7"
          onClick={addRow}
          disabled={disabled}
        >
          <Plus className="h-3.5 w-3.5" />
          Add
        </Button>
      </div>

      {value.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          Passed to the tests as environment variables for this run only.
        </p>
      ) : (
        <div className="space-y-2">
          {value.map((row, index) => (
            <div
              key={index}
              className="grid grid-cols-[minmax(0,1fr),minmax(0,1fr),32px] items-center gap-2"
            >
              <Input
                value={row.name}
                onChange={(e) => updateRow(index, { name: e.target.value })}
                placeholder="NAME"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                disabled={disabled}
                className="h-8 font-mono text-xs"
              />
              <Input
                value={row.value}
                onChange={(e) => updateRow(index, { value: e.target.value })}
                placeholder="value"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                disabled={disabled}
                className="h-8 font-mono text-xs"
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-7 w-7"
                onClick={() => removeRow(index)}
                disabled={disabled}
                aria-label={`Remove parameter ${row.name || index + 1}`}
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
