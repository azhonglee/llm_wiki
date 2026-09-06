import { describe, expect, it } from "vitest"
import { openFileNavigation, returnFromFile, shouldApplyFileResult } from "./workspace-navigation"

describe("workspace file navigation", () => {
  it("从搜索等视图打开文件时记录返回视图", () => {
    expect(openFileNavigation("search", "wiki")).toEqual({ activeView: "wiki", returnView: "search" })
  })

  it("在 Wiki 内切换文件不会制造无效返回历史", () => {
    expect(openFileNavigation("wiki", "wiki")).toEqual({ activeView: "wiki", returnView: null })
    expect(returnFromFile(null, "wiki")).toBe("wiki")
  })

  it("只应用最新文件请求的响应", () => {
    expect(shouldApplyFileResult(2, 2, "wiki/b.md", "wiki/b.md")).toBe(true)
    expect(shouldApplyFileResult(2, 1, "wiki/b.md", "wiki/a.md")).toBe(false)
  })
})
