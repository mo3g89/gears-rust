import { toast } from 'sonner';
import { useEnvironments, useDeleteEnvironment } from '@/api/hooks';
import { EnvironmentsTable } from '@/components/environments/EnvironmentsTable';
import { CreateEnvironmentDialog } from '@/components/environments/CreateEnvironmentDialog';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { useConfirm } from '@/components/ui/confirm-dialog';
import { Loader2 } from 'lucide-react';

export function EnvironmentsPage() {
  const { data: environments, isLoading, error } = useEnvironments();
  const deleteEnvironment = useDeleteEnvironment();
  const confirm = useConfirm();

  const handleDelete = async (name: string) => {
    const ok = await confirm({
      title: `Delete environment "${name}"?`,
      description: 'This environment will be removed permanently.',
      confirmText: 'Delete',
      variant: 'destructive',
    });
    if (ok) {
      deleteEnvironment.mutate(name, {
        onSuccess: () => {
          toast.success('Environment deleted successfully');
        },
        onError: (error) => {
          toast.error('Failed to delete environment', {
            description: String(error),
          });
        },
      });
    }
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="text-center py-8">
        <p className="text-destructive">Failed to load environments</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Environments</h1>
          {/*
            Was "Manage Kubernetes platforms for test execution". Kubernetes is
            one product's substrate, not the environment's -- and this sits above
            the very table Task 21 made descriptor-driven (re-review, N-6).
          */}
          <p className="text-muted-foreground">
            Manage the environments tests run against
          </p>
        </div>
        <CreateEnvironmentDialog />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Configured Environments</CardTitle>
          <CardDescription>
            {environments?.length || 0} environment{environments?.length !== 1 ? 's' : ''} configured
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <EnvironmentsTable
            environments={environments || []}
            onDelete={handleDelete}
            isDeleting={deleteEnvironment.isPending}
          />
        </CardContent>
      </Card>
    </div>
  );
}
