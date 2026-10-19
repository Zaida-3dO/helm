// Builds the bench page (index.html) against the UI sources. `npm run perf -w checks` builds it.
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const here = fileURLToPath(new URL('.', import.meta.url));

export default defineConfig({
  root: here,
  plugins: [react()],
  clearScreen: false,
  logLevel: 'warn',
  build: { target: 'es2022', outDir: fileURLToPath(new URL('./dist', import.meta.url)), emptyOutDir: true, sourcemap: false },
});
