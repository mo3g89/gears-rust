import { ReactNode } from 'react';
import { Package } from 'lucide-react';
import { useProducts } from '@/api/hooks';
import { useSelectedProduct } from '@/lib/selectedProduct';

/**
 * Guards product-scoped pages. The active product is auto-selected in AppShell
 * (first product by default), so in normal flow this just waits a tick for that
 * selection to resolve. It renders a dedicated empty state only when no products
 * exist at all.
 */
export function RequireProduct({ children }: { children: ReactNode }) {
  const [selectedId] = useSelectedProduct();
  const { data: products } = useProducts();

  if (products && products.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-24 text-center text-muted-foreground">
        <Package className="h-10 w-10 mb-4 opacity-50" />
        <h2 className="text-lg font-medium text-foreground">Нет продуктов</h2>
        <p className="mt-1 text-sm">
          Создайте продукт в каталоге, чтобы увидеть планы, тесты и раны.
        </p>
      </div>
    );
  }

  const valid = !!selectedId && !!products && products.some((p) => p.id === selectedId);
  if (!valid) {
    // Products still loading or auto-select in flight — avoid flashing a gate.
    return null;
  }

  return <>{children}</>;
}
