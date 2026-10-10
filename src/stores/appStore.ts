import { create } from "zustand";

import type { AppRoute } from "../app/routes";
import { flushBeforeLeavingEditor } from "../lib/navigation";
import type { ExportKind } from "../types/export";

export interface ExportCompletion {
  kind: ExportKind;
  outputPath: string;
  translatedCount: number;
  missingCount: number;
  namespace: string;
  autoOpenOutputFolder: boolean;
}

type SettingsReturnRoute = Exclude<AppRoute, "settings">;

interface AppStore {
  route: AppRoute;
  settingsReturnRoute: SettingsReturnRoute;
  settingsCategory: "general" | "ai" | "editor" | "appearance" | "advanced";
  exportCompletion: ExportCompletion | null;
  goTo: (route: AppRoute) => Promise<void>;
  openSettings: (category?: AppStore["settingsCategory"]) => Promise<void>;
  closeSettings: () => Promise<void>;
  setExportCompletion: (completion: ExportCompletion) => void;
}

export const useAppStore = create<AppStore>()((set, get) => ({
  route: "home",
  settingsReturnRoute: "home",
  settingsCategory: "general",
  exportCompletion: null,
  goTo: async (route) => {
    if (route === "settings") {
      await get().openSettings();
      return;
    }
    const currentRoute = get().route;
    if (currentRoute === "editor" && route !== "editor") {
      const flushed = await flushBeforeLeavingEditor();
      if (!flushed) return;
    }
    set({ route, ...(route === "export" ? { exportCompletion: null } : {}) });
  },
  openSettings: async (category = "general") => {
    const currentRoute = get().route;
    if (currentRoute === "settings") {
      set({ settingsCategory: category });
      return;
    }
    if (currentRoute === "editor" && !(await flushBeforeLeavingEditor())) return;
    set({
      route: "settings",
      settingsReturnRoute: currentRoute,
      settingsCategory: category,
    });
  },
  closeSettings: async () => get().goTo(get().settingsReturnRoute),
  setExportCompletion: (exportCompletion) => set({ exportCompletion }),
}));
