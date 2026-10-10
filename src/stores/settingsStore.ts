import { create } from "zustand";

import { getSettings, saveSettings } from "../lib/tauri";
import type { AppSettingsDto } from "../types/settings";

const APPEARANCE_THEME_CACHE_KEY = "yakucraft.appearance.theme";
type AppearanceTheme = "light" | "dark" | "system";

function isAppearanceTheme(value: unknown): value is AppearanceTheme {
  return value === "light" || value === "dark" || value === "system";
}

function readCachedAppearanceTheme(): AppearanceTheme {
  if (typeof window === "undefined") return "system";
  try {
    const cached = window.localStorage.getItem(APPEARANCE_THEME_CACHE_KEY);
    return isAppearanceTheme(cached) ? cached : "system";
  } catch {
    return "system";
  }
}

function cacheAppearanceTheme(value: unknown) {
  if (typeof window === "undefined") return;
  try {
    if (isAppearanceTheme(value)) {
      window.localStorage.setItem(APPEARANCE_THEME_CACHE_KEY, value);
    } else {
      window.localStorage.removeItem(APPEARANCE_THEME_CACHE_KEY);
    }
  } catch {
    // 設定DBを正として扱い、WebView cacheが使えなくても保存を続ける。
  }
}

export const SETTING_DEFAULTS: Record<string, unknown> = {
  "general.recentProjectsCount": 10,
  "general.defaultExportDirectory": "",
  "general.autoOpenOutputFolder": false,
  "updates.checkOnStartup": true,
  "editor.fontSize": 13,
  "editor.rowDensity": "standard",
  "editor.wrapSource": false,
  "editor.wrapTarget": false,
  "editor.tableColumnWidths": {
    preview: 48,
    key: 220,
    source: 240,
    target: 280,
    state: 140,
    namespace: 170,
  },
  "editor.inspectorWidth": 320,
  "editor.modTabPosition": "top",
  "editor.modSidebarWidth": 256,
  "editor.modSidebarCollapsed": false,
  "editor.hideInspectorPreview": false,
  "editor.confirmBatchOverwrite": true,
  "appearance.theme": readCachedAppearanceTheme(),
  "advanced.debugLogs": false,
  "ai.provider": "chatgpt",
  "ai.privacyNoticeAccepted": false,
};

interface SettingsStore {
  settings: AppSettingsDto;
  loaded: boolean;
  error: string | null;
  load: () => Promise<void>;
  save: (patch: Record<string, unknown>) => Promise<void>;
  value: <T>(key: string) => T;
}

function defaultSettings(): AppSettingsDto {
  return { schemaVersion: 1, ...SETTING_DEFAULTS };
}

export const useSettingsStore = create<SettingsStore>()((set, get) => ({
  settings: defaultSettings(),
  loaded: false,
  error: null,
  load: async () => {
    try {
      const settings = await getSettings();
      const loadedSettings = { ...defaultSettings(), ...settings };
      if (!isAppearanceTheme(loadedSettings["appearance.theme"])) {
        loadedSettings["appearance.theme"] = "system";
      }
      cacheAppearanceTheme(loadedSettings["appearance.theme"]);
      set({ settings: loadedSettings, loaded: true, error: null });
    } catch {
      set({ loaded: true, error: "設定を読み込めませんでした。" });
    }
  },
  save: async (patch) => {
    if (isAppearanceTheme(patch["appearance.theme"])) {
      const previousTheme = get().settings["appearance.theme"];
      const nextTheme = patch["appearance.theme"];
      // テーマは選択直後に反映できるよう、DB応答を待つ間は画面と初期描画cacheを先行更新する。
      set((state) => ({ settings: { ...state.settings, ...patch }, error: null }));
      cacheAppearanceTheme(nextTheme);
      try {
        await saveSettings(patch);
      } catch (error) {
        set((state) => state.settings["appearance.theme"] === nextTheme
          ? { settings: { ...state.settings, "appearance.theme": previousTheme } }
          : {});
        if (get().settings["appearance.theme"] === previousTheme) {
          cacheAppearanceTheme(previousTheme);
        }
        throw error;
      }
      return;
    }

    await saveSettings(patch);
    set((state) => ({ settings: { ...state.settings, ...patch }, error: null }));
  },
  value: <T,>(key: string) => {
    const value = get().settings[key];
    return (value ?? SETTING_DEFAULTS[key]) as T;
  },
}));
