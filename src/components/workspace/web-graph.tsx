import { useEffect, useMemo } from "react"
import Graph from "graphology"
import { SigmaContainer, useLoadGraph, useRegisterEvents } from "@react-sigma/core"
import "@react-sigma/core/lib/style.css"
import type { GraphData } from "@/api/contracts"

const NODE_COLORS: Record<string, string> = {
  source: "#f59e0b",
  concept: "#8b5cf6",
  entity: "#3b82f6",
  query: "#10b981",
}

function GraphLoader({ data, onOpen }: { data: GraphData; onOpen: (path: string) => void }) {
  const loadGraph = useLoadGraph()
  const registerEvents = useRegisterEvents()
  const graph = useMemo(() => {
    const next = new Graph({ multi: true })
    const count = Math.max(data.nodes.length, 1)
    data.nodes.forEach((node, index) => {
      const angle = (index / count) * Math.PI * 2
      next.addNode(node.id, {
        label: node.label ?? node.path ?? node.id,
        x: Math.cos(angle),
        y: Math.sin(angle),
        size: Math.max(5, Math.min(14, 5 + (node.linkCount ?? 0))),
        color: NODE_COLORS[node.type ?? ""] ?? "#64748b",
        path: node.path ?? node.id,
      })
    })
    data.edges.forEach((edge, index) => {
      if (!next.hasNode(edge.source) || !next.hasNode(edge.target)) return
      const key = edge.id ?? `${edge.source}-${edge.target}-${index}`
      if (!next.hasEdge(key)) next.addEdgeWithKey(key, edge.source, edge.target, { size: Math.max(1, edge.weight ?? 1), color: "#94a3b8" })
    })
    return next
  }, [data])

  useEffect(() => loadGraph(graph), [graph, loadGraph])
  useEffect(() => registerEvents({ clickNode: ({ node }) => onOpen(String(graph.getNodeAttribute(node, "path"))) }), [graph, onOpen, registerEvents])
  return null
}

export function WebGraph({ data, onOpen }: { data: GraphData; onOpen: (path: string) => void }) {
  return (
    <SigmaContainer
      className="h-full w-full bg-background"
      settings={{ renderEdgeLabels: false, labelDensity: 0.12, labelGridCellSize: 90, defaultEdgeColor: "#94a3b8", allowInvalidContainer: true }}
    >
      <GraphLoader data={data} onOpen={onOpen} />
    </SigmaContainer>
  )
}
