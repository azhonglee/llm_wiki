import { describe, expect, it } from "vitest"
import { isTauriRuntime } from "./runtime"

describe("isTauriRuntime", () => {
  it("recognizes both supported Tauri globals", () => {
    expect(isTauriRuntime({ __TAURI_INTERNALS__: {} })).toBe(true)
    expect(isTauriRuntime({ __TAURI__: {} })).toBe(true)
  })

  it("keeps a regular browser on the web application path", () => {
    expect(isTauriRuntime({})).toBe(false)
  })
})
