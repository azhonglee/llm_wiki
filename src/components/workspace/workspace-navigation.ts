export function openFileNavigation<T extends string>(currentView: T, wikiView: T): {
  activeView: T
  returnView: T | null
} {
  return {
    activeView: wikiView,
    returnView: currentView === wikiView ? null : currentView,
  }
}

export function returnFromFile<T extends string>(returnView: T | null, wikiView: T): T {
  return returnView ?? wikiView
}

export function shouldApplyFileResult(
  currentGeneration: number,
  requestGeneration: number,
  currentPath: string | null,
  requestPath: string,
): boolean {
  return currentGeneration === requestGeneration && currentPath === requestPath
}
