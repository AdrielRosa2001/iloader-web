import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

// @ts-expect-error process is a nodejs global
const apiTarget = process.env.ILOADER_API ?? "http://127.0.0.1:8080";

const platform = (file: string) =>
  fileURLToPath(new URL(`./src/platform/${file}`, import.meta.url));

// The web build replaces every Tauri module with a browser implementation
// talking to iloader-server (keep in sync with "paths" in tsconfig.json).
const tauriShims: Record<string, string> = {
  "@tauri-apps/api/core": platform("core.ts"),
  "@tauri-apps/api/event": platform("event.ts"),
  "@tauri-apps/api/app": platform("app.ts"),
  "@tauri-apps/plugin-dialog": platform("dialog.ts"),
  "@tauri-apps/plugin-opener": platform("opener.ts"),
  "@tauri-apps/plugin-store": platform("store.ts"),
  "@tauri-apps/plugin-updater": platform("updater.ts"),
  "@tauri-apps/plugin-process": platform("process.ts"),
};

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],

  resolve: {
    alias: Object.entries(tauriShims).map(([find, replacement]) => ({
      find: new RegExp(`^${find.replace(/[/.]/g, "\\$&")}$`),
      replacement,
    })),
  },

  build: {
    target: "es2018",
  },

  clearScreen: false,
  // `npm run dev` serves the UI on :1420 and forwards the API (and the
  // WebSocket) to a running iloader-server (ILOADER_API, default :8080).
  server: {
    port: 1420,
    strictPort: true,
    proxy: {
      "/api": {
        target: apiTarget,
        ws: true,
      },
    },
    watch: {
      ignored: ["**/src-tauri/**", "**/target/**", "**/crates/**"],
    },
  },
}));
