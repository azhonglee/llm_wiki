import React from "react"
import ReactDOM from "react-dom/client"
import "./index.css"
import "@/i18n"
import { isTauriRuntime } from "@/web/runtime"

function applyBrowserTheme() {
  const preference = localStorage.getItem("llm-wiki:theme") ?? "system"
  const theme = preference === "dark" || (preference === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches)
    ? "dark"
    : "light"
  document.documentElement.classList.remove("light", "dark")
  document.documentElement.classList.add(theme)
  document.documentElement.dataset.theme = preference
}

function renderStartupError(error: unknown) {
  console.error("[startup] failed to initialize LLM Wiki:", error)
  const root = document.getElementById("root")
  if (!root) return
  const message = error instanceof Error ? (error.stack ?? error.message) : String(error)
  root.innerHTML = `
    <div style="font-family: system-ui, sans-serif; padding: 24px; color: #111; color: light-dark(#111, #f5f5f5); background: #fff; background: light-dark(#fff, #111); min-height: 100vh; color-scheme: light dark;">
      <h1 style="font-size: 18px; margin: 0 0 12px;">LLM Wiki failed to start</h1>
      <p style="margin: 0 0 12px;">The frontend startup code threw an error before React could render.</p>
      <pre style="white-space: pre-wrap; border: 1px solid light-dark(#ddd, #333); border-radius: 8px; padding: 12px; background: light-dark(#f7f7f7, #1d1d1d);">${message.replace(/[&<>"']/g, (ch) => ({
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        '"': "&quot;",
        "'": "&#39;",
      }[ch] ?? ch))}</pre>
    </div>
  `
}

async function initApp() {
  try {
    const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement)
    if (isTauriRuntime(window)) {
      const [{ default: App }, { AppDialogHost }, theme] = await Promise.all([
        import("./App"),
        import("@/components/app-dialog-host"),
        import("@/lib/theme"),
      ])
      if (navigator.userAgent.includes("Mac OS X")) {
        document.documentElement.classList.add("platform-macos")
      }
      await theme.loadAndApplyTheme()
      theme.watchSystemTheme()
      root.render(
        <React.StrictMode>
          <App />
          <AppDialogHost />
        </React.StrictMode>,
      )
      return
    }

    applyBrowserTheme()
    const { WebApp } = await import("@/web/web-app")
    root.render(
      <React.StrictMode>
        <WebApp />
      </React.StrictMode>,
    )
  } catch (error) {
    renderStartupError(error)
  }
}

void initApp()
