import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { viteSingleFile } from "vite-plugin-singlefile";

// The production build is a single self-contained index.html: every
// script and stylesheet is inlined, so the Rust renderer can embed it and
// inject the page data without any external requests.
export default defineConfig({
  plugins: [react(), tailwindcss(), viteSingleFile()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // `public/conseqa.json` is development data only; the embedded
    // bundle carries its data injected by the Rust renderer.
    copyPublicDir: false,
  },
});
