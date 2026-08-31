import { defineConfig } from 'vitest/config';
import path from 'node:path';

// The adapters under test are pure functions over wire shapes: no DOM, no React,
// no network. So this config deliberately keeps the default `node` environment
// rather than pulling in jsdom — a test that needed a DOM would be testing a
// component, which is not what `src/api/adapters.ts` holds.
//
// `@` is aliased here as well as in `vite.config.ts` and `tsconfig.json`, because
// vitest resolves imports through its own config, not the app's.
export default defineConfig({
  resolve: {
    alias: { '@': path.resolve(__dirname, './src') },
  },
  test: {
    include: ['src/**/*.test.ts'],
  },
});
