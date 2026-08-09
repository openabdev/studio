import { defineConfig } from "vite";

// `base: "./"` so the built `dist/index.html` opens standalone over file://
// (the browser-console / preview use case) with no dev server.
export default defineConfig({
  base: "./",
  build: { outDir: "dist", emptyOutDir: true },
});
