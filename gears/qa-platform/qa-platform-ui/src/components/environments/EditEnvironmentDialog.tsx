import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { toast } from 'sonner';
import {
  useProducts,
  useRenameEnvironment,
  useUpdateEnvironment,
} from '@/api/hooks';
import { EnvironmentInfo } from '@/api/types';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import { Combobox } from '@/components/ui/combobox';
import { Pencil } from 'lucide-react';

interface EditEnvironmentDialogProps {
  environment: EnvironmentInfo;
  trigger?: React.ReactNode;
}

export function EditEnvironmentDialog({ environment, trigger }: EditEnvironmentDialogProps) {
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState(environment.name);
  const [description, setDescription] = useState(environment.description || '');
  const [productId, setProductId] = useState(environment.product_id || '');
  const [isDefault, setIsDefault] = useState(environment.is_default);
  const updateEnvironment = useUpdateEnvironment();
  const renameEnvironment = useRenameEnvironment();
  const { data: products } = useProducts();

  useEffect(() => {
    if (open) {
      setName(environment.name);
      setDescription(environment.description || '');
      setProductId(environment.product_id || '');
      setIsDefault(environment.is_default);
    }
  }, [open, environment.description, environment.is_default, environment.name, environment.product_id]);

  const submitting = updateEnvironment.isPending || renameEnvironment.isPending;

  const finishSubmit = (finalName: string) => {
    updateEnvironment.mutate(
      {
        name: finalName,
        data: {
          description: description.trim(),
          product_id: productId,
          is_default: isDefault,
        },
      },
      {
        onSuccess: () => {
          toast.success(`Environment "${finalName}" updated`);
          setOpen(false);
        },
        onError: (error) => {
          toast.error('Failed to update environment', {
            description: String(error),
          });
        },
      }
    );
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = name.trim();
    if (!trimmed) {
      toast.error('Environment name cannot be empty');
      return;
    }
    if (trimmed === environment.name) {
      finishSubmit(environment.name);
      return;
    }
    renameEnvironment.mutate(
      { name: environment.name, newName: trimmed },
      {
        onSuccess: (result) => {
          toast.success(`Environment renamed to "${result.name}"`);
          // Re-route the URL if the page was looking at the old name.
          if (window.location.pathname.startsWith(`/environments/${encodeURIComponent(environment.name)}`)) {
            navigate(`/environments/${encodeURIComponent(result.name)}`, { replace: true });
          }
          finishSubmit(result.name);
        },
        onError: (error) => {
          toast.error('Failed to rename environment', { description: String(error) });
        },
      }
    );
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        {trigger || (
          <Button variant="outline" size="sm">
            <Pencil className="h-4 w-4 mr-2" />
            Edit
          </Button>
        )}
      </DialogTrigger>
      <DialogContent>
        <form onSubmit={handleSubmit}>
          <DialogHeader>
            <DialogTitle>Edit Environment</DialogTitle>
            <DialogDescription>
              Update metadata for <span className="font-medium">{environment.name}</span>
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="name">Name</Label>
              <Input
                id="name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={environment.name}
              />
              <p className="font-mono text-[10px] text-muted-foreground" title="Internal UUID — stable across renames">
                id: {environment.id}
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="description">Description</Label>
              <Textarea
                id="description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="Environment description"
                className="min-h-[120px]"
              />
            </div>

            <div className="space-y-2">
              <Label htmlFor="product">Product</Label>
              <Combobox
                id="product"
                value={productId}
                onChange={setProductId}
                options={[
                  { value: '', label: 'No product' },
                  ...(products || []).map((product) => ({
                    value: product.id,
                    label: `${product.name} (${product.key})`,
                  })),
                ]}
                placeholder="No product"
              />
            </div>

            <div className="flex items-start justify-between gap-4 rounded-md border border-border/60 p-3">
              <div className="space-y-1">
                <Label htmlFor="is-default">Default environment for this product</Label>
                <p className="text-xs text-muted-foreground">
                  What the &quot;Default cluster&quot; option runs against in the Run Plan and
                  Schedule dialogs. Turning this on clears the flag on whichever environment
                  currently holds it for this product.
                </p>
              </div>
              <Switch
                id="is-default"
                checked={isDefault}
                onCheckedChange={setIsDefault}
                disabled={!productId}
              />
            </div>
            {!productId && (
              <p className="text-xs text-muted-foreground">
                An environment with no product cannot be a product&apos;s default.
              </p>
            )}

            <p className="text-xs text-muted-foreground">
              Version and build are not observed in this deployment: nothing here reads an
              environment's install metadata, so both stay empty.
            </p>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={submitting}>
              {submitting ? 'Saving...' : 'Save Changes'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
