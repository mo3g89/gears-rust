import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'path'

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 3000,
    proxy: {
      // Two retargets, both of which `npm run dev` was broken without.
      //
      // The prefix: the client's API_BASE_URL is `/qa/v1` (src/api/client.ts),
      // not `/api`, since Task 9 -- so this rule matched nothing and every dev
      // request fell through to the dev server itself, which answered
      // index.html.
      //
      // The port: 8080 was the legacy backend's. It is the UI container's own
      // published port now, so proxying to it would send the dev server either
      // to nothing or to the *built* image it exists to replace. 8087 is where
      // the gears answer on the host -- the same port `npm run gen:api` reads
      // /openapi.json from.
      '/qa/v1': {
        target: 'http://localhost:8087',
        changeOrigin: true,
      },
    },
  },
})
