export function isTauriRuntime(target: object): boolean {
  return "__TAURI_INTERNALS__" in target || "__TAURI__" in target
}
