import { useEffect, useState } from 'react';
import { useCreatePlatform, useActiveProduct, useProducts } from '@/api/hooks';
import { toast } from 'sonner';
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
import { Textarea } from '@/components/ui/textarea';
import { Combobox } from '@/components/ui/combobox';
import { Plus } from 'lucide-react';

export function CreatePlatformDialog() {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  const [kubeconfig, setKubeconfig] = useState('');
  const [productId, setProductId] = useState('');

  const createPlatform = useCreatePlatform();
  const activeProduct = useActiveProduct();
  const { data: products } = useProducts();

  // Default the selector to the currently active product when the dialog opens.
  useEffect(() => {
    if (open) {
      setProductId(activeProduct?.id || '');
    }
  }, [open, activeProduct?.id]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!name || !kubeconfig) {
      toast.error('Name and kubeconfig are required');
      return;
    }

    let base64Kubeconfig: string;
    try {
      base64Kubeconfig = btoa(kubeconfig.trim());
    } catch (error) {
      toast.error('Failed to encode kubeconfig');
      return;
    }

    createPlatform.mutate(
      {
        name: name.trim(),
        kubeconfig: base64Kubeconfig,
        product_id: productId || undefined,
      },
      {
        onSuccess: () => {
          toast.success('Platform created successfully');
          setOpen(false);
          setName('');
          setKubeconfig('');
          setProductId('');
        },
        onError: (error) => {
          toast.error('Failed to create platform', {
            description: String(error),
          });
        },
      }
    );
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="mr-2 h-4 w-4" />
          Add Platform
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-hidden flex flex-col">
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <DialogHeader>
            <DialogTitle>Create New Platform</DialogTitle>
            <DialogDescription>
              Add a Kubernetes platform by providing a name and kubeconfig file
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 overflow-y-auto flex-1 px-1 py-4">
            <div className="space-y-2">
              <Label htmlFor="name">Platform Name</Label>
              <Input
                id="name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="my-platform"
                required
              />
              <p className="text-xs text-muted-foreground">
                A unique name for this Kubernetes platform
              </p>
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
              <p className="text-xs text-muted-foreground">
                The product this platform belongs to
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="kubeconfig">Kubeconfig</Label>
              <Textarea
                id="kubeconfig"
                value={kubeconfig}
                onChange={(e) => setKubeconfig(e.target.value)}
                placeholder="Paste your kubeconfig YAML content here..."
                className="font-mono text-sm min-h-[200px]"
                required
              />
              <p className="text-xs text-muted-foreground">
                Paste the contents of your kubeconfig file (YAML format)
              </p>
            </div>

            <p className="text-xs text-muted-foreground">
              Version, build and Kubernetes namespace are auto-detected from the
              vpadm install metadata (<code>core-install-metadata</code> ConfigMap)
              once the platform is reachable.
            </p>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setOpen(false)}
            >
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={!name || !kubeconfig || createPlatform.isPending}
            >
              {createPlatform.isPending ? 'Creating...' : 'Create Platform'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
