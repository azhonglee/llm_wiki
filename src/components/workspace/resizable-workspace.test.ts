import { describe, expect, it } from "vitest"
import { clampPanelWidth, isCompactWorkspace } from "./resizable-workspace"

describe("clampPanelWidth", () => {
  it("限制左右面板的拖拽范围", () => {
    expect(clampPanelWidth(20, "left")).toBe(180)
    expect(clampPanelWidth(320, "left")).toBe(320)
    expect(clampPanelWidth(900, "left")).toBe(440)
    expect(clampPanelWidth(20, "right")).toBe(280)
    expect(clampPanelWidth(900, "right")).toBe(560)
  })

  it("窄窗口使用抽屉布局", () => {
    expect(isCompactWorkspace(390)).toBe(true)
    expect(isCompactWorkspace(768)).toBe(true)
    expect(isCompactWorkspace(900)).toBe(false)
  })
})
