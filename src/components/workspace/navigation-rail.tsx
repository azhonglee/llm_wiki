import type { LucideIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip"

export interface WorkspaceNavItem<T extends string> {
  id: T
  label: string
  icon: LucideIcon
  disabled?: boolean
  badge?: number
  hint?: string
}

interface NavigationRailProps<T extends string> {
  active: T
  primary: WorkspaceNavItem<T>[]
  secondary: WorkspaceNavItem<T>[]
  onSelect: (id: T) => void
  brand?: React.ReactNode
}

export function NavigationRail<T extends string>({
  active,
  primary,
  secondary,
  onSelect,
  brand,
}: NavigationRailProps<T>) {
  const renderItem = (item: WorkspaceNavItem<T>) => {
    const Icon = item.icon
    const button = (
      <TooltipTrigger
        key={item.id}
        type="button"
        aria-label={item.label}
        aria-current={active === item.id ? "page" : undefined}
        aria-disabled={item.disabled || undefined}
        disabled={item.disabled}
        onClick={() => onSelect(item.id)}
        className={cn(
          "relative flex size-10 items-center justify-center rounded-md text-muted-foreground transition-colors",
          "hover:bg-accent hover:text-accent-foreground disabled:cursor-not-allowed disabled:opacity-35",
          active === item.id && "bg-accent text-accent-foreground",
        )}
      >
        <Icon className="size-5" />
        {!!item.badge && item.badge > 0 && (
          <span className="absolute right-0.5 top-0.5 min-w-3.5 rounded-full bg-destructive px-1 text-center text-[9px] leading-3.5 text-white">
            {item.badge > 99 ? "99+" : item.badge}
          </span>
        )}
      </TooltipTrigger>
    )
    return (
      <Tooltip key={item.id}>
        {button}
        <TooltipContent side="right">
          {item.label}{item.hint ? ` · ${item.hint}` : ""}
        </TooltipContent>
      </Tooltip>
    )
  }

  return (
    <TooltipProvider>
      <nav className="flex w-12 shrink-0 flex-col items-center border-r bg-muted/30 py-1.5" aria-label="工作台导航">
        <div className="mb-1 flex size-10 items-center justify-center">{brand}</div>
        <div className="flex flex-col gap-0.5">{primary.map(renderItem)}</div>
        <div className="mt-auto flex flex-col gap-0.5">{secondary.map(renderItem)}</div>
      </nav>
    </TooltipProvider>
  )
}
