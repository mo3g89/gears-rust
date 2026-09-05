// @vitest-environment jsdom
//
// Task 23: the product-plugin selector. `CreateProductForm` gained a required
// `plugin_instance_id`, and until this page could send one every product
// create from the UI was a guaranteed 400 -- the gear's `qa_products` column
// is `NOT NULL` since Task 20a. This suite exercises the real dialog against
// a mocked API client so a regression that stops sending the field (or lets
// create through without one) fails here, not just at the API.
//
// `.test.ts`, not `.test.tsx` -- `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so `createElement` stands in for JSX exactly as
// `EnvironmentsTable.test.ts` and `RunsPage.test.ts` already do.
import { createElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

// `vi.mock`'s factory is hoisted above every module-level `const`, so
// anything it references has to go through `vi.hoisted` -- a plain `const`
// here would be read before it is initialised (EnvironmentsTable.test.ts hit
// this). One object keeps every fixture the mock factory and the edit test
// need, rather than splitting across several `vi.hoisted` calls that would
// then have to rely on their relative order.
const FIXTURES = vi.hoisted(() => {
  const pluginId = 'gts.a~acme.v1';
  // A second, distinct plugin so the edit tests can tell "the product's own
  // plugin" from "any plugin the catalogue happens to list first" (I-4): a
  // fixture with only one registered plugin can't distinguish "picked
  // correctly" from "picked arbitrarily", so a bug that pre-fills from the
  // catalogue instead of from the product would pass unnoticed.
  const secondPluginId = 'gts.a~globex.v1';
  return {
    pluginId,
    secondPluginId,
    productId: 'product-1',
    plugins: [
      { instance_id: pluginId, vendor: 'Acme Corp', credential_schema: [], observed_schema: [] },
      { instance_id: secondPluginId, vendor: 'Globex Inc', credential_schema: [], observed_schema: [] },
    ],
    // An existing product bound to the *second* plugin, not the first --
    // deliberately, so a pre-fill that defaults to "whatever the catalogue
    // lists first" fails this fixture instead of passing it by accident.
    existingProduct: {
      id: 'product-1',
      name: 'Existing Product',
      key: 'EXIST',
      description: 'An existing product',
      folder: 'exist',
      plugin_instance_id: secondPluginId,
      created_at: '2026-08-01T00:00:00Z',
      updated_at: '2026-08-01T00:00:00Z',
    },
    // The DTO `apiPost`/`apiPut` resolve with. Its content doesn't matter
    // beyond being a valid `ProductDto` -- `productFromDto` reads it and
    // `useCreateProduct`/`useUpdateProduct`'s `onSuccess` only invalidates
    // the products query.
    createdDto: {
      id: 'product-new',
      name: 'New Product',
      key: 'NEW',
      description: '',
      folder: null,
      plugin_instance_id: pluginId,
      created_at: '2026-09-01T00:00:00Z',
      updated_at: '2026-09-01T00:00:00Z',
    },
  };
});

vi.mock('@/api/client', () => ({
  // Default catalogue: no existing products. The edit test swaps this in
  // place (by path, not `mockImplementationOnce` -- that replaces only the
  // next call, whichever one it is, which is the trap EnvironmentsTable.test.ts
  // documents) and restores it afterwards.
  apiGet: vi.fn(async (path: string) => {
    if (path === '/products') return [];
    if (path === '/product-plugins') return FIXTURES.plugins;
    return [];
  }),
  apiGetBlob: vi.fn(),
  apiPost: vi.fn(async () => FIXTURES.createdDto),
  apiPut: vi.fn(async () => FIXTURES.createdDto),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
  setTokenProvider: vi.fn(),
}));

vi.mock('sonner', () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

import { apiGet, apiPost, apiPut } from '@/api/client';
import { ConfirmProvider } from '@/components/ui/confirm-dialog';
import { toast } from 'sonner';
import { ProductsPage } from './ProductsPage';

function renderPage() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(
        MemoryRouter,
        null,
        createElement(ConfirmProvider, null, createElement(ProductsPage, null))
      )
    )
  );
  return client;
}

afterEach(() => {
  cleanup();
  vi.mocked(apiPost).mockClear();
  vi.mocked(apiPut).mockClear();
  vi.mocked(toast.error).mockClear();
});

describe('ProductsPage — create requires a plugin', () => {
  it('sends the selected plugin_instance_id in the create payload', async () => {
    renderPage();

    fireEvent.click(await screen.findByRole('button', { name: /add product/i }));

    fireEvent.change(screen.getByLabelText('Product Name *'), { target: { value: 'Widget' } });
    fireEvent.change(screen.getByLabelText('Product Key *'), { target: { value: 'WIDGET' } });

    // Open the combobox and pick the one registered plugin. The trigger
    // button's accessible *name* comes from its `<Label for="plugin">`
    // ("Product Plugin *"), not its visible text -- a labelled form control's
    // label wins the accname computation over its own content -- so it has
    // to be found by that text rather than by role name.
    fireEvent.click(screen.getByText('Select a product plugin').closest('button')!);
    fireEvent.click(await screen.findByRole('button', { name: 'Acme Corp' }));

    fireEvent.click(screen.getByRole('button', { name: 'Create Product' }));

    await waitFor(() => expect(apiPost).toHaveBeenCalled());
    const [path, body] = vi.mocked(apiPost).mock.calls[0];
    expect(path).toBe('/products');
    expect((body as Record<string, unknown>).plugin_instance_id).toBe(FIXTURES.pluginId);
  });

  // The three degraded states m-2 added. Each existed because loading, a failed
  // catalogue read and "registered but empty" otherwise look identical to the
  // operator -- an unpickable dropdown plus "Please fill in all required
  // fields", pointing at a field there is nothing to be done about. The
  // re-review measured that the whole block was untested: gutting it to a
  // no-op compiled clean and left all 219 tests green.
  it('says the catalogue is empty rather than leaving an unexplained dead field', async () => {
    const client = vi.mocked(apiGet);
    const original = client.getMockImplementation();
    client.mockImplementation(async (path: string) => {
      if (path === '/product-plugins') return [];
      return [];
    });
    try {
      renderPage();
      fireEvent.click(await screen.findByRole('button', { name: /add product/i }));
      expect(
        await screen.findByText(/no product plugins are registered in this deployment/i)
      ).toBeTruthy();
    } finally {
      client.mockImplementation(original!);
    }
  });

  it('names the catalogue as what failed, rather than blaming a required field', async () => {
    const client = vi.mocked(apiGet);
    const original = client.getMockImplementation();
    client.mockImplementation(async (path: string) => {
      if (path === '/product-plugins') throw new Error('boom');
      return [];
    });
    try {
      renderPage();
      fireEvent.click(await screen.findByRole('button', { name: /add product/i }));
      expect(
        await screen.findByText(/could not load the product plugin catalogue/i)
      ).toBeTruthy();
    } finally {
      client.mockImplementation(original!);
    }
  });

  it('does not submit when no plugin is selected', async () => {
    renderPage();

    fireEvent.click(await screen.findByRole('button', { name: /add product/i }));

    fireEvent.change(screen.getByLabelText('Product Name *'), { target: { value: 'Widget' } });
    fireEvent.change(screen.getByLabelText('Product Key *'), { target: { value: 'WIDGET' } });
    // No plugin selected.

    fireEvent.click(screen.getByRole('button', { name: 'Create Product' }));

    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Please fill in all required fields'));
    expect(apiPost).not.toHaveBeenCalled();
  });
});

describe('ProductsPage — edit preserves the existing binding', () => {
  // Both tests below open the edit dialog on `FIXTURES.existingProduct`, bound
  // to the *second* plugin, against a catalogue that also lists a different
  // first one -- so any assertion that would also pass for "the first plugin"
  // is a false pass (I-4). `withEditDialog` swaps `/products` to return the
  // one existing product (by path, not `mockImplementationOnce`, which
  // replaces only the very next `apiGet` call whichever one it is -- the trap
  // EnvironmentsTable.test.ts documents) and restores the default afterwards.
  async function withEditDialog(run: () => Promise<void>) {
    const mockedApiGet = vi.mocked(apiGet);
    const original = mockedApiGet.getMockImplementation();
    mockedApiGet.mockImplementation(async (path: string) => {
      if (path === '/products') return [FIXTURES.existingProduct];
      if (path === '/product-plugins') return FIXTURES.plugins;
      return [];
    });
    try {
      renderPage();
      fireEvent.click(await screen.findByRole('button', { name: 'Edit product' }));
      await run();
    } finally {
      // `mockImplementation` isn't scoped to one call -- it outlives this test
      // unless it's put back (the same trap `mockImplementationOnce` sets, one
      // door over).
      mockedApiGet.mockImplementation(original!);
    }
  }

  it("pre-selects the product's own plugin, and resubmitting unchanged omits the field", async () => {
    await withEditDialog(async () => {
      // The dialog pre-fills from the product's current `plugin_instance_id` --
      // the combobox trigger shows that plugin's label, not the placeholder,
      // once the plugin catalogue has resolved. (Found by text, not role
      // name: the trigger's accessible name is its `<Label>`'s text --
      // "Product Plugin *" -- regardless of what it currently displays.) It
      // has to be the *second* plugin's label specifically -- the first
      // plugin's label is never rendered anywhere else on this page, so its
      // absence is as informative as the second's presence.
      expect(await screen.findByText('Globex Inc')).toBeTruthy();
      expect(screen.queryByText('Acme Corp')).toBeNull();

      fireEvent.click(screen.getByRole('button', { name: 'Save Product' }));

      await waitFor(() => expect(apiPut).toHaveBeenCalled());
      const [path, body] = vi.mocked(apiPut).mock.calls[0];
      expect(path).toBe(`/products/${FIXTURES.productId}`);
      // Ruling G-2: qa-catalog re-validates *any* `Some(id)` it receives
      // against the live per-process plugin registry -- it does not compare
      // it to what's stored -- so resending the product's own current
      // binding on an edit that never touched it is a rebind request, not a
      // no-op, and 400s for a product whose stored plugin this deployment
      // doesn't register. An edit that doesn't change the selection must
      // therefore omit the key entirely (D-18's "`None` means leave the
      // binding alone"), not resend the same value.
      expect((body as Record<string, unknown>).plugin_instance_id).toBeUndefined();
    });
  });

  it('sends the new plugin_instance_id when the selection actually changes on edit', async () => {
    await withEditDialog(async () => {
      expect(await screen.findByText('Globex Inc')).toBeTruthy();

      // Deliberately rebind to the first plugin.
      fireEvent.click(screen.getByText('Globex Inc').closest('button')!);
      fireEvent.click(await screen.findByRole('button', { name: 'Acme Corp' }));

      fireEvent.click(screen.getByRole('button', { name: 'Save Product' }));

      await waitFor(() => expect(apiPut).toHaveBeenCalled());
      const [, body] = vi.mocked(apiPut).mock.calls[0];
      // The other half of G-2: a selection that genuinely differs from the
      // stored binding is a deliberate rebind and must be sent, not omitted.
      expect((body as Record<string, unknown>).plugin_instance_id).toBe(FIXTURES.pluginId);
    });
  });
});
