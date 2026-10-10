import { invokeApp } from "../../lib/tauri";

export function getModIcon(modUnitId: number) {
  return invokeApp<number[] | null>("get_mod_icon", { modUnitId });
}

export function clearModIconCache() {
  return invokeApp<number[] | null>("get_mod_icon", { modUnitId: null, clearCache: true });
}
