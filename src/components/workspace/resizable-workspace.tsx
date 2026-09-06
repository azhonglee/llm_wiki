import { useCallback, useEffect, useRef, useState } from "react"
import { PanelLeftOpen, PanelRightOpen } from "lucide-react"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"

export const LEFT_PANEL_MIN = 180
export const LEFT_PANEL_MAX = 440
export const RIGHT_PANEL_MIN = 280
export const RIGHT_PANEL_MAX = 560
const COMPACT_BREAKPOINT = 900

export function isCompactWorkspace(width: number): boolean {
  return width < COMPACT_BREAKPOINT
}
const LEFT_WIDTH_KEY = "llm-wiki:web-left-panel-width"
const RIGHT_WIDTH_KEY = "llm-wiki:web-right-panel-width"

function storedWidth(key: string, fallback: number, side: "left" | "right"): number {
  if (typeof window === "undefined") return clampPanelWidth(fallback, side)
  const parsed = Number(window.localStorage.getItem(key))
  return clampPanelWidth(Number.isFinite(parsed) && parsed > 0 ? parsed : fallback, side)
}

export function clampPanelWidth(width: number, side: "left" | "right"): number {
  const min = side === "left" ? LEFT_PANEL_MIN : RIGHT_PANEL_MIN
  const max = side === "left" ? LEFT_PANEL_MAX : RIGHT_PANEL_MAX
  return Math.min(max, Math.max(min, width))
}

interface ResizableWorkspaceProps {
  left: React.ReactNode
  children: React.ReactNode
  right?: React.ReactNode
  leftCollapsed: boolean
  rightOpen: boolean
  onLeftCollapsedChange: (collapsed: boolean) => void
  onRightOpenChange: (open: boolean) => void
  initialLeftWidth?: number
  initialRightWidth?: number
}

export function ResizableWorkspace({
  left,
  children,
  right,
  leftCollapsed,
  rightOpen,
  onLeftCollapsedChange,
  onRightOpenChange,
  initialLeftWidth = 248,
  initialRightWidth = 380,
}: ResizableWorkspaceProps) {
  const [leftWidth, setLeftWidth] = useState(() => storedWidth(LEFT_WIDTH_KEY, initialLeftWidth, "left"))
  const [rightWidth, setRightWidth] = useState(() => storedWidth(RIGHT_WIDTH_KEY, initialRightWidth, "right"))
  const [compact, setCompact] = useState(() => typeof window !== "undefined" && isCompactWorkspace(window.innerWidth))
  const dragRef = useRef<{ side: "left" | "right"; startX: number; startWidth: number } | null>(null)

  const stopDragging = useCallback(() => {
    dragRef.current = null
    document.body.style.cursor = ""
    document.body.style.userSelect = ""
  }, [])

  useEffect(() => {
    const move = (event: PointerEvent) => {
      const drag = dragRef.current
      if (!drag) return
      const delta = event.clientX - drag.startX
      if (drag.side === "left") setLeftWidth(clampPanelWidth(drag.startWidth + delta, "left"))
      else setRightWidth(clampPanelWidth(drag.startWidth - delta, "right"))
    }
    window.addEventListener("pointermove", move)
    window.addEventListener("pointerup", stopDragging)
    return () => {
      window.removeEventListener("pointermove", move)
      window.removeEventListener("pointerup", stopDragging)
    }
  }, [stopDragging])

  useEffect(() => window.localStorage.setItem(LEFT_WIDTH_KEY, String(leftWidth)), [leftWidth])
  useEffect(() => window.localStorage.setItem(RIGHT_WIDTH_KEY, String(rightWidth)), [rightWidth])

  useEffect(() => {
    const reconcile = () => {
      const nextCompact = isCompactWorkspace(window.innerWidth)
      setCompact(nextCompact)
      if (nextCompact) {
        onLeftCollapsedChange(true)
        onRightOpenChange(false)
      }
    }
    reconcile()
    window.addEventListener("resize", reconcile)
    return () => window.removeEventListener("resize", reconcile)
  }, [onLeftCollapsedChange, onRightOpenChange])

  const startDragging = (side: "left" | "right", event: React.PointerEvent) => {
    event.preventDefault()
    dragRef.current = {
      side,
      startX: event.clientX,
      startWidth: side === "left" ? leftWidth : rightWidth,
    }
    document.body.style.cursor = "col-resize"
    document.body.style.userSelect = "none"
  }

  if (compact) {
    return (
      <div className="relative flex min-h-0 min-w-0 flex-1 overflow-hidden">
        <main className="min-h-0 min-w-0 flex-1">{children}</main>
        {!leftCollapsed ? (
          <>
            <button type="button" aria-label="关闭文件面板" className="absolute inset-0 z-20 bg-black/20" onClick={() => onLeftCollapsedChange(true)} />
            <aside className="absolute inset-y-0 left-0 z-30 w-[min(85vw,320px)] overflow-hidden border-r bg-background shadow-xl">{left}</aside>
          </>
        ) : (
          <Button variant="outline" size="icon-xs" className="absolute left-2 top-2 z-20 bg-background/90" onClick={() => onLeftCollapsedChange(false)} title="展开文件面板"><PanelLeftOpen /></Button>
        )}
        {rightOpen && right ? (
          <>
            <button type="button" aria-label="关闭活动面板" className="absolute inset-0 z-20 bg-black/20" onClick={() => onRightOpenChange(false)} />
            <aside className="absolute inset-y-0 right-0 z-30 w-[min(90vw,380px)] overflow-hidden border-l bg-background shadow-xl">{right}</aside>
          </>
        ) : right ? (
          <Button variant="outline" size="icon-xs" className="absolute right-2 top-2 z-20 bg-background/90" onClick={() => onRightOpenChange(true)} title="展开活动面板"><PanelRightOpen /></Button>
        ) : null}
      </div>
    )
  }

  return (
    <div className="relative flex min-h-0 min-w-0 flex-1 overflow-hidden">
      {!leftCollapsed ? (
        <>
          <aside className="min-h-0 shrink-0 overflow-hidden border-r" style={{ width: leftWidth }}>
            {left}
          </aside>
          <div
            role="separator"
            aria-label="调整文件面板宽度"
            aria-orientation="vertical"
            onPointerDown={(event) => startDragging("left", event)}
            className="z-10 -ml-0.5 w-1 cursor-col-resize bg-transparent hover:bg-primary/35"
          />
        </>
      ) : (
        <Button
          variant="outline"
          size="icon-xs"
          className="absolute left-2 top-2 z-20 bg-background/90"
          onClick={() => onLeftCollapsedChange(false)}
          title="展开文件面板"
        >
          <PanelLeftOpen />
        </Button>
      )}

      <main className={cn("min-h-0 min-w-0 flex-1", leftCollapsed && "pl-0")}>{children}</main>

      {rightOpen && right ? (
        <>
          <div
            role="separator"
            aria-label="调整活动面板宽度"
            aria-orientation="vertical"
            onPointerDown={(event) => startDragging("right", event)}
            className="z-10 -mr-0.5 w-1 cursor-col-resize bg-transparent hover:bg-primary/35"
          />
          <aside className="min-h-0 shrink-0 overflow-hidden border-l" style={{ width: rightWidth }}>
            {right}
          </aside>
        </>
      ) : right ? (
        <Button
          variant="outline"
          size="icon-xs"
          className="absolute right-2 top-2 z-20 bg-background/90"
          onClick={() => onRightOpenChange(true)}
          title="展开活动面板"
        >
          <PanelRightOpen />
        </Button>
      ) : null}
    </div>
  )
}
