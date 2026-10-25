/// <reference types="node" />
import { cp, rm } from 'node:fs/promises';
import { defineConfig } from 'vitest/config';
import type { Plugin } from 'vite';
import react from '@vitejs/plugin-react';

// `adk web` embeds its own copy inside the published CLI crate. Keep that
// copy in sync with successful production builds, including removed hashes.
function vendorStudio(): Plugin {
  let failed = false;
  return {
    name: 'vendor-studio',
    apply: 'build',
    buildEnd(error) {
      failed = error !== undefined;
    },
    renderError() {
      failed = true;
    },
    async closeBundle() {
      if (failed) return;
      const source = new URL('../gemini-adk-web-rs/static/studio/', import.meta.url);
      const target = new URL('../../tools/gemini-adk-cli-rs/assets/web/studio/', import.meta.url);
      await rm(target, { recursive: true, force: true });
      await cp(source, target, { recursive: true });
    },
  };
}

// The build lands in the web app's static tree, so the Rust server serves
// the Studio without Node installed. `npm run dev` proxies the API to a
// running `cargo run -p gemini-adk-web-rs`.
export default defineConfig({
  plugins: [react(), vendorStudio()],
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
      '/static/js/audio.js': 'http://127.0.0.1:25125',
      '/static/worklets': 'http://127.0.0.1:25125',
    },
  },
  test: {
    globals: true,
    environment: 'node',
  },
});
