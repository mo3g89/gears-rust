import { useEffect, useState } from 'react';
import { Outlet } from 'react-router-dom';
import { Sidebar } from './Sidebar';
import { Header } from './Header';
import { useProducts } from '@/api/hooks';
import { useSelectedProduct } from '@/lib/selectedProduct';

export function AppShell() {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [selectedId, setSelectedId] = useSelectedProduct();
  const { data: products } = useProducts();

  // Default to the first product (and repair a stale id pointing at a deleted
  // product). With a single product this selects it immediately.
  useEffect(() => {
    if (!products || products.length === 0) return;
    const valid = selectedId && products.some((p) => p.id === selectedId);
    if (!valid) setSelectedId(products[0].id);
  }, [products, selectedId, setSelectedId]);

  return (
    <div className="min-h-screen bg-background">
      {/* Sidebar */}
      <Sidebar isOpen={sidebarOpen} onClose={() => setSidebarOpen(false)} />

      {/* Overlay for mobile */}
      {sidebarOpen && (
        <div
          className="fixed inset-0 z-40 bg-black/50 lg:hidden"
          onClick={() => setSidebarOpen(false)}
        />
      )}

      {/* Main content area */}
      <div className="lg:pl-64">
        <Header onMenuClick={() => setSidebarOpen(true)} />
        
        <main className="p-4">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
