declare const process: { env: Record<string, string | undefined> }; // (node runs this file; no @types/node here)
import { build, defineConfig, type Plugin } from "vite-plus";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// frame.js: the script of web games' frames (src/frame.ts), a classic script (it must run before
// the game's own scripts), served by ippai serve's game listener
const frame = (): Plugin => ({
  name: "ippai-frame",
  apply: "build",
  async closeBundle() {
    await build({
      configFile: false,
      logLevel: "warn",
      build: {
        outDir: "dist",
        emptyOutDir: false,
        target: "es2022",
        lib: { entry: "src/frame.ts", formats: ["iife"], name: "ippaiFrame", fileName: () => "frame.js" },
      },
    });
  },
});

// Built into web/dist and served by ippai serve next to the store. The UI is React with
// Tailwind CSS and shadcn/ui (src/components/ui); sake's files (public/sake/) are copied as-is.
export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss(), frame()],
  resolve: { alias: { "@": new URL("./src", import.meta.url).pathname } },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // index.html = play.html: sake (crates/sake, files in public/sake/), the game list
    // without ?run=;
    // bench.html: bench.exe on play.html with the native numbers next to it; logs.html: the runs'
    // logs (crash reports)
    rollupOptions: {
      input: {
        main: "index.html",
        play: "play.html",
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
