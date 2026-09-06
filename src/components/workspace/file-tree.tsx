import { useState } from "react"
import { ChevronDown, ChevronRight, File, Folder } from "lucide-react"
import type { FileTreeNode } from "@/api/contracts"
import { cn } from "@/lib/utils"

export function isDirectoryNode(node: FileTreeNode): boolean {
  return node.isDir || node.kind === "directory"
}

export function findTreeNode(nodes: FileTreeNode[], path: string): FileTreeNode | null {
  for (const node of nodes) {
    if (node.path === path) return node
    const found = node.children ? findTreeNode(node.children, path) : null
    if (found) return found
  }
  return null
}

export function sourceTree(nodes: FileTreeNode[]): FileTreeNode[] {
  const source = findTreeNode(nodes, "raw/sources") ?? findTreeNode(nodes, "/raw/sources")
  return source?.children ?? []
}

export function wikiTree(nodes: FileTreeNode[]): FileTreeNode[] {
  const wiki = findTreeNode(nodes, "wiki") ?? findTreeNode(nodes, "/wiki")
  return wiki?.children ?? []
}

export function WorkspaceFileTree({
  nodes,
  selectedPath,
  onOpen,
  depth = 0,
}: {
  nodes: FileTreeNode[]
  selectedPath: string | null
  onOpen: (path: string) => void
  depth?: number
}) {
  return (
    <>
      {nodes.map((node) => (
        <TreeItem key={node.path} node={node} selectedPath={selectedPath} onOpen={onOpen} depth={depth} />
      ))}
    </>
  )
}

function TreeItem({
  node,
  selectedPath,
  onOpen,
  depth,
}: {
  node: FileTreeNode
  selectedPath: string | null
  onOpen: (path: string) => void
  depth: number
}) {
  const directory = isDirectoryNode(node)
  const [expanded, setExpanded] = useState(depth < 2)
  return (
    <div>
      <button
        type="button"
        onClick={() => directory ? setExpanded((value) => !value) : onOpen(node.path)}
        className={cn(
          "flex w-full items-center gap-1 rounded px-1.5 py-1 text-left text-sm hover:bg-muted",
          selectedPath === node.path && "bg-muted font-medium",
        )}
        style={{ paddingLeft: `${depth * 12 + 6}px` }}
      >
        {directory ? expanded ? <ChevronDown className="size-3.5 shrink-0" /> : <ChevronRight className="size-3.5 shrink-0" /> : <span className="w-3.5" />}
        {directory ? <Folder className="size-3.5 shrink-0 text-muted-foreground" /> : <File className="size-3.5 shrink-0 text-muted-foreground" />}
        <span className="truncate">{node.name}</span>
      </button>
      {directory && expanded && node.children && (
        <WorkspaceFileTree nodes={node.children} selectedPath={selectedPath} onOpen={onOpen} depth={depth + 1} />
      )}
    </div>
  )
}
