import { useProducts } from '@/api/hooks';
import { useSelectedProduct } from '@/lib/selectedProduct';
import { setSelectedBranch } from '@/lib/selectedBranch';
import { Combobox } from '@/components/ui/combobox';
import { cn } from '@/lib/utils';

/**
 * Global product scope selector. Selecting a product scopes the entire UI; it
 * also resets the selected branch, since branches belong to the product's
 * repository.
 */
export function ProductSwitcher({ className }: { className?: string }) {
  const { data: products } = useProducts();
  const [selectedId, setSelectedId] = useSelectedProduct();

  const options = (products ?? []).map((p) => ({ value: p.id, label: p.name }));

  const handleChange = (id: string) => {
    setSelectedId(id);
    setSelectedBranch('');
  };

  return (
    <Combobox
      value={selectedId}
      onChange={handleChange}
      options={options}
      placeholder="Select product…"
      emptyText="No products"
      className={cn('w-full', className)}
    />
  );
}
