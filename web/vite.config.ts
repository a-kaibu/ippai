declare const process: { env: Record<string, string | undefined> }; // (node runs this file; no @types/node here)
import { defineConfig } from "vite-plus";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Built into web/dist and served by vnvm serve next to the store. The UI is React with
// Tailwind CSS and shadcn/ui (src/components/ui); sake's files (public/sake/) are copied as-is.
export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": new URL("./src", import.meta.url).pathname } },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // index.html = wine.html: Wine built for wasm (host/, files in public/wine/), the game list
    // without ?run=;
    // bench.html: bench.exe on wine.html with the native numbers next to it; logs.html: the runs'
    // logs (crash reports)
    rollupOptions: {
      input: {
        main: "index.html",
        wine: "wine.html",
        bench: "bench.html",
        game: "game.html",
        logs: "logs.html",
      },
    },
  },
  server: {
    headers: {
      "cross-origin-opener-policy": "same-origin",
      "cross-origin-embedder-policy": "require-corp",
    },
    proxy: { "/store": process.env.VNVM_SERVER ?? "http://127.0.0.1:8080" },
  },
  lint: { ignorePatterns: ["public/**"], options: { typeAware: true, typeCheck: true } },
  fmt: { printWidth: 110, ignorePatterns: ["public/**"] },
});
