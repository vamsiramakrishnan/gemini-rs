import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// The build lands in the web app's static tree, so the Rust server serves
// the Studio without Node installed. `npm run dev` proxies the API to a
// running `cargo run -p gemini-adk-web-rs`.
export default defineConfig({
  plugins: [react()],
  base: '/static/studio/',
  build: {
    outDir: '../gemini-adk-web-rs/static/studio',
    emptyOutDir: true,
    chunkSizeWarningLimit: 1500,
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:25125',
      '/ws': { target: 'ws://127.0.0.1:25125', ws: true },
      '/static/examples': 'http://127.0.0.1:25125',
    },
  },
  test: {
    globals: true,
    environment: 'node',
  },
});
