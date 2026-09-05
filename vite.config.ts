import path from "path"
import { readFileSync } from "fs"
import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"

const host = process.env.TAURI_DEV_HOST
const webDevHost = process.env.LLM_WIKI_WEB_DEV_HOST
const webApiTarget = process.env.LLM_WIKI_WEB_DEV_API ?? "http://127.0.0.1:19828"

// Read version from package.json at config-load time so the Settings
// UI can show the running app version without duplicating the string.
const pkgJson = JSON.parse(readFileSync(path.resolve(__dirname, "package.json"), "utf-8"))

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },

  define: {
    __APP_VERSION__: JSON.stringify(pkgJson.version),
  },

  // Keep the Tauri dev-server contract only when `tauri dev` supplies its
  // host. A regular browser dev server must be reachable independently.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || webDevHost || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    proxy: {
      "/api": {
        target: webApiTarget,
        changeOrigin: false,
      },
    },
    watch: { ignored: ["**/src-tauri/**"] },
  },

  test: {
    environment: "node",
    // Loads .env.test.local into process.env for real-LLM tests.
    // The loader itself is a no-op if the file is absent, so this is
    // safe to keep on for every test run.
    setupFiles: ["./src/test-helpers/load-test-env.ts"],
  },
}))
