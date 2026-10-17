import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The dev server address is fixed: the host trusts exactly this origin in debug builds
// (host/crates/ipc/src/authority.rs).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { host: '127.0.0.1', port: 1420, strictPort: true },
  build: { target: 'es2022', outDir: 'dist', emptyOutDir: true, sourcemap: false },
});
