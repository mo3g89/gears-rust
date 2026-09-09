import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Loader2, Plus, Trash2 } from 'lucide-react';
import type { PipelineVariable, PipelineVariablesConfig } from '@/api/types';
import { Label } from '@/components/ui/label';
import { Input } from '@/components/ui/input';
import { Button } from '@/components/ui/button';
import { UnavailableNotice } from '@/components/ui/unavailable';

/**
 * No `secure` flag: this deployment has no secured-variable concept, so the editor
 * does not offer one. See `REMOVED-SURFACES.md` (Task 8a, C10) — a variable is
 * stored and returned in cleartext, and the padlock/masking that used to sit here
 * claimed a protection the backend never applied.
 */
function createEmptyVariable(): PipelineVariable {
  return { name: '', value: '' };
}

function normalizeVariable(variable: PipelineVariable): PipelineVariable {
  return { name: variable.name.trim(), value: variable.value };
}

interface VariablesEditorProps {
  data: PipelineVariablesConfig | undefined;
  isLoading: boolean;
  /** Receives the FULL list (added/removed rows applied) and persists it.
   *  Resolves with the server's view, so the editor can show what the server
   *  actually stored. */
  onSave: (next: PipelineVariablesConfig) => Promise<PipelineVariablesConfig | void>;
  isSaving: boolean;
  /** Shown above the "No variables configured" empty state when set. */
  emptyHint?: string;
}

/**
 * Shared add-row + list-with-delete UI for both global pipeline variables
 * (Settings) and per-environment variables (EnvironmentDetailPage). Backend wire
 * shape is identical, so the same component handles both.
 */
export function VariablesEditor({
  data,
  isLoading,
  onSave,
  isSaving,
  emptyHint,
}: VariablesEditorProps) {
  const [variables, setVariables] = useState<PipelineVariable[]>([]);
  const [newVariable, setNewVariable] = useState<PipelineVariable>(createEmptyVariable());
  const [activeAction, setActiveAction] = useState<string | null>(null);

  useEffect(() => {
    if (data) {
      setVariables(data.variables);
    }
  }, [data]);

  const addVariable = async () => {
    const normalized = normalizeVariable(newVariable);
    if (!normalized.name) {
      toast.error('Variable name is required');
      return;
    }
    setActiveAction('add');
    try {
      const result = await onSave({ variables: [...variables, normalized] });
      const finalList = result?.variables ?? [...variables, normalized];
      setVariables(finalList);
      setNewVariable(createEmptyVariable());
      toast.success('Variable added');
    } catch (err) {
      toast.error('Failed to add variable', { description: String(err) });
    } finally {
      setActiveAction(null);
    }
  };

  const deleteVariable = async (index: number) => {
    setActiveAction(`delete-${index}`);
    try {
      const next = variables.filter((_, variableIndex) => variableIndex !== index);
      const result = await onSave({ variables: next });
      setVariables(result?.variables ?? next);
      toast.success('Variable deleted');
    } catch (err) {
      toast.error('Failed to delete variable', { description: String(err) });
    } finally {
      setActiveAction(null);
    }
  };

  if (isLoading) {
    return (
      <div className="flex h-32 items-center justify-center">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <UnavailableNotice title="Secured variables are not available in this deployment">
        A variable is stored and returned in cleartext — the variables endpoint serves its
        value verbatim to anyone who can read it, and nothing here masks or encrypts it. Do
        not put a credential, token or other secret in a variable.
      </UnavailableNotice>

      <div className="grid grid-cols-1 gap-2 rounded-lg border p-3 lg:grid-cols-[minmax(0,1fr),minmax(0,1fr),80px] lg:items-end">
        <div className="space-y-1">
          <Label htmlFor="new-variable-name" className="text-xs">Name</Label>
          <Input
            id="new-variable-name"
            value={newVariable.name}
            onChange={(e) => setNewVariable((current) => ({ ...current, name: e.target.value }))}
            placeholder="Name"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            disabled={isSaving}
            className="h-8"
          />
        </div>

        <div className="space-y-1">
          <Label htmlFor="new-variable-value" className="text-xs">Value</Label>
          <Input
            id="new-variable-value"
            value={newVariable.value}
            onChange={(e) => setNewVariable((current) => ({ ...current, value: e.target.value }))}
            placeholder="Value"
            autoComplete="off"
            disabled={isSaving}
            className="h-8"
          />
        </div>

        <Button onClick={addVariable} disabled={isSaving} size="sm" className="h-8">
          {activeAction === 'add' ? <Loader2 className="h-4 w-4 animate-spin" /> : <Plus className="h-4 w-4" />}
          Add
        </Button>
      </div>

      <div className="overflow-hidden rounded-lg border">
        {variables.length === 0 ? (
          <div className="space-y-1 p-3 text-center text-xs text-muted-foreground">
            <div>No variables configured yet.</div>
            {emptyHint ? <div>{emptyHint}</div> : null}
          </div>
        ) : (
          variables.map((variable, index) => {
            const isDeleting = activeAction === `delete-${index}` && isSaving;
            return (
              <div
                key={`${variable.name}-${index}`}
                className="grid grid-cols-1 gap-2 border-t px-3 py-2 first:border-t-0 md:grid-cols-[220px,minmax(0,1fr),60px] md:items-center"
              >
                <div className="text-xs font-medium text-foreground break-all">{variable.name}</div>
                <div className="text-xs text-foreground break-all md:pr-2">
                  {variable.value || <span className="text-muted-foreground">Empty value</span>}
                </div>
                <div className="flex items-center justify-start md:justify-end">
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7"
                    onClick={() => deleteVariable(index)}
                    disabled={isSaving}
                    aria-label={`Delete variable ${variable.name}`}
                  >
                    {isDeleting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Trash2 className="h-3.5 w-3.5" />}
                  </Button>
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
