import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Sprint-6 — Vite config for the edge_monitor web companion.
//
// `build.outDir` is `dist/` (default); the Rust `rust-embed` wrapper
// reads from `web/dist/` at compile time. Keep these aligned — a
// rename here without updating `src/web/assets.rs` ships a binary
// with no embedded UI.
//
// Asset paths are relative (`base: './'`) so the dashboard works
// when served from any path on the axum router, including the root
// `GET /`.

export default defineConfig({
    plugins: [svelte()],
    base: './',
    build: {
        outDir: 'dist',
        emptyOutDir: true,
        // Produce a single JS bundle + single CSS bundle. The dashboard
        // is small enough that code-splitting adds complexity without
        // payoff.
        rollupOptions: {
            output: {
                inlineDynamicImports: true,
                entryFileNames: 'assets/[name].js',
                chunkFileNames: 'assets/[name].js',
                assetFileNames: 'assets/[name].[ext]',
            },
        },
    },
    server: {
        // Local dev: Vite proxy passes API calls through to the
        // running Rust binary on 7070 so `npm run dev` works without
        // CORS gymnastics.
        port: 5173,
        proxy: {
            '/api': {
                target: 'http://127.0.0.1:7070',
                changeOrigin: true,
                ws: true,
            },
        },
    },
});
