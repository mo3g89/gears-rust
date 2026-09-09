import { usePipelineVariables, useUpdatePipelineVariables } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { VariablesEditor } from '@/components/settings/VariablesEditor';

export function SettingsVariablesPage() {
  const { data, isLoading } = usePipelineVariables();
  const update = useUpdatePipelineVariables();

  return (
    <Card>
      <CardHeader>
        <CardTitle>Pipeline Variables</CardTitle>
        <CardDescription>
          Variables are injected as environment variables into every test run.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="space-y-3 text-sm text-muted-foreground">
          <p>
            Repository-style variables can be used by every run. Reference them in tests with the
            environment variable name directly.
          </p>
        </div>

        <VariablesEditor
          data={data}
          isLoading={isLoading}
          isSaving={update.isPending}
          onSave={(next) => update.mutateAsync(next)}
        />
      </CardContent>
    </Card>
  );
}
