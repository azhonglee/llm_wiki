import { describe, expect, it } from "vitest"
import type { FileTreeNode } from "@/api/contracts"
import { findTreeNode, sourceTree, wikiTree } from "./file-tree"

const tree: FileTreeNode[] = [
  {
    name: "raw",
    path: "raw",
    kind: "directory",
    isDir: true,
    children: [
      {
        name: "sources",
        path: "raw/sources",
        kind: "directory",
        isDir: true,
        children: [{ name: "a.pdf", path: "raw/sources/a.pdf", kind: "file", isDir: false }],
      },
    ],
  },
  {
    name: "wiki",
    path: "wiki",
    kind: "directory",
    isDir: true,
    children: [{ name: "index.md", path: "wiki/index.md", kind: "file", isDir: false }],
  },
]

describe("workspace file tree helpers", () => {
  it("定位嵌套节点", () => expect(findTreeNode(tree, "raw/sources/a.pdf")?.name).toBe("a.pdf"))
  it("只返回 raw/sources 子树", () => expect(sourceTree(tree).map((node) => node.path)).toEqual(["raw/sources/a.pdf"]))
  it("只返回 wiki 子树", () => expect(wikiTree(tree).map((node) => node.path)).toEqual(["wiki/index.md"]))
})
