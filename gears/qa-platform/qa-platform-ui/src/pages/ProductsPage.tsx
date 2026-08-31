import { useState } from 'react';
import { Link } from 'react-router-dom';
import { toast } from 'sonner';
import { useProducts, useDeleteProduct, useCreateProduct, useUpdateProduct } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { useConfirm } from '@/components/ui/confirm-dialog';
import { Loader2, Pencil, Plus, Trash2 } from 'lucide-react';

function deriveTestsFolder(productKey: string): string {
  const normalized = productKey
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');

  return normalized || 'tests';
}

export function ProductsPage() {
  const { data: products, isLoading, error } = useProducts();
  const deleteProduct = useDeleteProduct();
  const createProduct = useCreateProduct();
  const updateProduct = useUpdateProduct();
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editingProductId, setEditingProductId] = useState<string | null>(null);
  const [formData, setFormData] = useState({ name: '', key: '', description: '', tests_folder: '' });
  const confirm = useConfirm();

  const openCreateDialog = () => {
    setEditingProductId(null);
    setFormData({ name: '', key: '', description: '', tests_folder: '' });
    setDialogOpen(true);
  };

  const openEditDialog = (id: string) => {
    const product = (products || []).find((item) => item.id === id);
    if (!product) return;
    setEditingProductId(id);
    setFormData({
      name: product.name,
      key: product.key,
      description: product.description,
      tests_folder: product.tests_folder,
    });
    setDialogOpen(true);
  };

  const handleDelete = async (id: string, name: string) => {
    const ok = await confirm({
      title: `Delete product "${name}"?`,
      description: 'This product will be removed permanently.',
      confirmText: 'Delete',
      variant: 'destructive',
    });
    if (ok) {
      deleteProduct.mutate(id, {
        onSuccess: () => toast.success('Product deleted'),
        onError: (err) => toast.error('Failed to delete product', { description: String(err) }),
      });
    }
  };

  const handleSave = () => {
    if (!formData.name || !formData.key) {
      toast.error('Please fill in all required fields');
      return;
    }

    const payload = {
      name: formData.name,
      key: formData.key,
      description: formData.description,
      tests_folder: editingProductId
        ? formData.tests_folder.trim() || deriveTestsFolder(formData.key)
        : deriveTestsFolder(formData.key),
    };

    if (editingProductId) {
      updateProduct.mutate(
        { id: editingProductId, data: payload },
        {
          onSuccess: () => {
            toast.success('Product updated');
            setDialogOpen(false);
            setEditingProductId(null);
          },
          onError: (err) => toast.error('Failed to update product', { description: String(err) }),
        }
      );
      return;
    }

    createProduct.mutate(payload, {
      onSuccess: () => {
        toast.success('Product created');
        setDialogOpen(false);
        setFormData({ name: '', key: '', description: '', tests_folder: '' });
      },
      onError: (err) => toast.error('Failed to create product', { description: String(err) }),
    });
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
        <p className="text-destructive">Failed to load products</p>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Product Catalog</h1>
          <p className="text-muted-foreground">Manage product definitions and related test assets</p>
        </div>
        <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
          <DialogTrigger asChild>
            <Button onClick={openCreateDialog}><Plus className="h-4 w-4 mr-2" /> Add Product</Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{editingProductId ? 'Edit Product' : 'Create Product'}</DialogTitle>
            </DialogHeader>
            <div className="space-y-4 py-4">
              <div className="space-y-2">
                <Label htmlFor="name">Product Name *</Label>
                <Input id="name" value={formData.name} onChange={(e) => setFormData({ ...formData, name: e.target.value })} placeholder="VHP Core" />
              </div>
              <div className="space-y-2">
                <Label htmlFor="key">Product Key *</Label>
                <Input
                  id="key"
                  value={formData.key}
                  onChange={(e) => {
                    const nextKey = e.target.value.toUpperCase();
                    setFormData((prev) => ({
                      ...prev,
                      key: nextKey,
                    }));
                  }}
                  placeholder="VHP"
                />
              </div>
              {editingProductId ? (
                <div className="space-y-2">
                  <Label htmlFor="tests-folder">Tests Folder *</Label>
                  <Input
                    id="tests-folder"
                    value={formData.tests_folder}
                    onChange={(e) => setFormData({ ...formData, tests_folder: e.target.value })}
                    placeholder="vhp"
                  />
                </div>
              ) : null}
              <div className="space-y-2">
                <Label htmlFor="description">Description</Label>
                <Input id="description" value={formData.description} onChange={(e) => setFormData({ ...formData, description: e.target.value })} placeholder="Product description" />
              </div>
              <Button onClick={handleSave} disabled={createProduct.isPending || updateProduct.isPending} className="w-full">
                {createProduct.isPending || updateProduct.isPending ? <Loader2 className="h-4 w-4 animate-spin mr-2" /> : null}
                {editingProductId ? 'Save Product' : 'Create Product'}
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Product Catalog</CardTitle>
          <CardDescription>{products?.length || 0} product{products?.length !== 1 ? 's' : ''} configured</CardDescription>
        </CardHeader>
        <CardContent>
          {!products?.length ? (
            <p className="text-sm text-muted-foreground text-center py-8">No products configured yet</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Key</TableHead>
                  <TableHead>Tests Folder</TableHead>
                  <TableHead>Created</TableHead>
                  <TableHead className="w-[80px] text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {products.map((product) => (
                  <TableRow key={product.id}>
                    <TableCell>
                      <Link
                        to={`/products/${product.id}`}
                        className="text-foreground/80 hover:text-foreground hover:underline"
                        title={product.description || undefined}
                      >
                        {product.name}
                      </Link>
                    </TableCell>
                    <TableCell>{product.key}</TableCell>
                    <TableCell className="text-muted-foreground font-mono text-xs">
                      <Link to={`/products/${product.id}`} className="hover:text-foreground hover:underline">
                        {product.tests_folder}
                      </Link>
                    </TableCell>
                    <TableCell className="text-muted-foreground">{new Date(product.created_at).toLocaleDateString()}</TableCell>
                    <TableCell className="text-right">
                      <div className="flex justify-end gap-1">
                        <Button variant="ghost" size="icon" className="h-7 w-7" onClick={() => openEditDialog(product.id)} title="Edit" aria-label="Edit product">
                          <Pencil className="h-4 w-4" />
                        </Button>
                        <Button variant="ghost" size="icon" className="h-7 w-7" onClick={() => handleDelete(product.id, product.name)} disabled={deleteProduct.isPending} title="Delete" aria-label="Delete product">
                          <Trash2 className="h-4 w-4 text-destructive" />
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
