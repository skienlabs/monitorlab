import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// `npm run dev` proxies the API to a hub running locally on :8443.
export default defineConfig({
  plugins: [react()],
  build: { outDir: "dist", sourcemap: false, chunkSizeWarningLimit: 900 },
  server: {
    proxy: {
      "/api": { target: "https://127.0.0.1:8443", secure: false, ws: true },
      "/download": { target: "https://127.0.0.1:8443", secure: false },
    },
  },
});
