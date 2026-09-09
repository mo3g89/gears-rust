import { useEffect, useState } from 'react';
import { useCreateEnvironment, useActiveProduct, useProducts } from '@/api/hooks';
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
import { Combobox } from '@/components/ui/combobox';
import { Plus } from 'lucide-react';
import { useProductPlugins } from '@/api/productPlugins';
import { pluginForProduct } from '@/lib/fieldDesc';
import {
  CredentialFields,
  credentialsPayload,
  missingRequired,
  type CredentialValues,
} from '@/components/environments/CredentialFields';

export function CreateEnvironmentDialog() {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  const [credentials, setCredentials] = useState<CredentialValues>({});
  const [productId, setProductId] = useState('');

  const createEnvironment = useCreateEnvironment();
  const activeProduct = useActiveProduct();
  const { data: products } = useProducts();
  const { data: productPlugins } = useProductPlugins();
  // The schema follows the product SELECTED IN THIS DIALOG, not the active one:
  // changing the selector has to change the fields, or the form would collect
  // one product's credentials and send them against another's plugin.
  const credentialSchema =
    pluginForProduct(
      productPlugins ?? [],
      (products ?? []).find((p) => p.id === productId)?.plugin_instance_id,
    )?.credential_schema ?? [];

  // Default the selector to the currently active product when the dialog opens.
  useEffect(() => {
    if (open) {
      setProductId(activeProduct?.id || '');
    }
  }, [open, activeProduct?.id]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!name) {
      toast.error('Name is required');
      return;
    }
    if (!productId) {
      // Required since Task 20b: the product is how the plugin resolves, and
      // without one the gear can neither observe this environment nor dispatch
      // against it. The 400 says the same thing; this says it sooner.
      toast.error('A product is required');
      return;
    }
    // The plugin's own required fields, named by the plugin. The gear enforces
    // this too (`require_declared_secrets`); this is the form saying it before
    // the round trip.
    const missing = missingRequired(credentialSchema, credentials);
    if (missing.length > 0) {
      toast.error(`${missing.join(', ')} ${missing.length === 1 ? 'is' : 'are'} required`);
      return;
    }

    createEnvironment.mutate(
      {
        name: name.trim(),
        credentials: credentialsPayload(credentialSchema, credentials),
        product_id: productId,
      },
      {
        onSuccess: () => {
          toast.success('Environment created successfully');
          setOpen(false);
          setName('');
          setCredentials({});
          setProductId('');
        },
        onError: (error) => {
          toast.error('Failed to create environment', {
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
          Add Environment
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-hidden flex flex-col">
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <DialogHeader>
            <DialogTitle>Create New Environment</DialogTitle>
            <DialogDescription>
              Add an environment by providing a name and its product's credentials
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 overflow-y-auto flex-1 px-1 py-4">
            <div className="space-y-2">
              <Label htmlFor="name">Environment Name</Label>
              <Input
                id="name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="my-environment"
                required
              />
              <p className="text-xs text-muted-foreground">
                A unique name for this environment
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="product">Product</Label>
              <Combobox
                id="product"
                value={productId}
                onChange={setProductId}
                options={(products || []).map((product) => ({
                  value: product.id,
                  label: `${product.name} (${product.key})`,
                }))}
                placeholder="Select a product"
              />
              {/*
                Required, and the placeholder says so rather than offering a
                "No product" option: a product selects the plugin whose
                `credential_schema()` generates the credential fields below,
                the submit handler refuses without one, and Task 20b made
                `qa_environments.product_id` `NOT NULL`. The rendered copy
                stays operator-facing -- this rationale is not for them
                (re-review, N-3).
              */}
              <p className="text-xs text-muted-foreground">
                The product this environment belongs to. Required — it determines
                which credentials this environment needs.
              </p>
            </div>

            {/*
              GENERATED FROM THE PRODUCT PLUGIN'S `credential_schema()`. This
              was a hardcoded "Kubeconfig" textarea -- VHP's single credential,
              with its name, in a form every product shares. VHP's form looks
              the same, because its plugin declares `kubeconfig` as
              `MultilineSecret` and that renders the same textarea.
            */}
            <CredentialFields
              schema={credentialSchema}
              values={credentials}
              onChange={setCredentials}
            />
            {/*
              The paragraph that stood here named VHP's detection mechanism --
              "vpadm install metadata (core-install-metadata ConfigMap)" -- to
              the operator of every product, in the dialog whose fields had just
              been made descriptor-driven so it would not. A product-specific
              explanation belongs in that product's own descriptors' `help`
              text: `CredentialFields` renders it for `credential_schema()`
              fields here, and `EnvironmentsTable`/`EnvironmentDetailPage`
              render it as a tooltip for `observed_schema()` fields.
            */}
            <p className="text-xs text-muted-foreground">
              Observed values are detected by this product's plugin once the
              environment is reachable.
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
              disabled={
                !name ||
                !productId ||
                missingRequired(credentialSchema, credentials).length > 0 ||
                createEnvironment.isPending
              }
            >
              {createEnvironment.isPending ? 'Creating...' : 'Create Environment'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
