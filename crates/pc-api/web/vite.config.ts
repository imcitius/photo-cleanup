import { defineConfig } from "vite";
export default defineConfig({
  build: {
    assetsDir: "static",
    rollupOptions: {
      output: {
        entryFileNames: "static/app.js",
        assetFileNames: "static/app.[ext]",
      },
    },
  },
  server: { proxy: { "/api": "http://127.0.0.1:8080" } },
});
