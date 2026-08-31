import { toast } from 'sonner';
import { usePlatforms, useDeletePlatform } from '@/api/hooks';
import { PlatformsTable } from '@/components/platforms/PlatformsTable';
import { CreatePlatformDialog } from '@/components/platforms/CreatePlatformDialog';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { useConfirm } from '@/components/ui/confirm-dialog';
import { Loader2 } from 'lucide-react';

export function PlatformsPage() {
  const { data: platforms, isLoading, error } = usePlatforms();
  const deletePlatform = useDeletePlatform();
  const confirm = useConfirm();

  const handleDelete = async (name: string) => {
    const ok = await confirm({
      title: `Delete platform "${name}"?`,
      description: 'This platform will be removed permanently.',
      confirmText: 'Delete',
      variant: 'destructive',
    });
    if (ok) {
      deletePlatform.mutate(name, {
        onSuccess: () => {
          toast.success('Platform deleted successfully');
        },
        onError: (error) => {
          toast.error('Failed to delete platform', {
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
        <p className="text-destructive">Failed to load platforms</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Platforms</h1>
          <p className="text-muted-foreground">
            Manage Kubernetes platforms for test execution
          </p>
        </div>
        <CreatePlatformDialog />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Configured Platforms</CardTitle>
          <CardDescription>
            {platforms?.length || 0} platform{platforms?.length !== 1 ? 's' : ''} configured
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <PlatformsTable
            platforms={platforms || []}
            onDelete={handleDelete}
            isDeleting={deletePlatform.isPending}
          />
        </CardContent>
      </Card>
    </div>
  );
}
