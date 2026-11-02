import { defineConfig } from "vite-plus";

// Built into web/dist and served by vnvm-server next to the store. Wine's files
// (public/wine/) are copied as-is by hangover/build.sh.
export default defineConfig({
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // index.html = wine.html: Wine built for wasm (hangover/, files in public/wine/), the game list
    // without ?run=;
    // bench.html: bench.exe on wine.html with the native numbers next to it; logs.html: the runs'
    // logs (crash reports)
    rollupOptions: { input: { main: "index.html", wine: "wine.html", bench: "bench.html", game: "game.html", logs: "logs.html" } },
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
