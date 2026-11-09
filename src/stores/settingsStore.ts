import { create } from "zustand";

import { getSettings, getSystemLocale, saveSettings } from "../lib/tauri";
import { detectTargetLocale, detectUiLanguage } from "../i18n/detect";
import { isUiLanguage, setUiLanguage, t } from "../i18n";
import { findMinecraftLanguage } from "../i18n/minecraftLanguages";
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
  // 既定は PLaMo。この PC 内だけで動き、無料で、アカウントも要らず、原文が PC の外へ出ない。
  // モデルとランタイムは自動取得しない（設定画面でダウンロードを選んだときだけ取得する）。
  "ai.provider": "plamo",
  "ai.privacyNoticeAccepted": false,
  "language.uiLanguage": "ja",
  "language.targetLocale": "en_us",
};

interface SettingsStore {
  settings: AppSettingsDto;
  /// バックエンドから読み込んだままの値。`settings` は既定値を混ぜた表示用なので、
  /// 「利用者が明示的に保存した」と「既定値のまま」を区別するにはこちらを見る。
  savedValues: Record<string, unknown>;
  loaded: boolean;
  error: string | null;
  systemLocale: string | null;
  load: () => Promise<void>;
  save: (patch: Record<string, unknown>) => Promise<void>;
  value: <T>(key: string) => T;
}

function defaultSettings(): AppSettingsDto {
  return { schemaVersion: 1, ...SETTING_DEFAULTS };
}

export const useSettingsStore = create<SettingsStore>()((set, get) => ({
  settings: defaultSettings(),
  savedValues: {},
  loaded: false,
  error: null,
  systemLocale: null,
  load: async () => {
    try {
      const settings = await getSettings();
      const systemLocale = await getSystemLocale();
      const loadedSettings = { ...defaultSettings(), ...settings };
      const uiLanguage = isUiLanguage(settings["language.uiLanguage"])
        ? settings["language.uiLanguage"]
        : detectUiLanguage(systemLocale);
      const configuredTarget = settings["language.targetLocale"];
      const configuredLocale = typeof configuredTarget === "string"
        ? configuredTarget.trim().replaceAll("-", "_")
        : "";
      const configuredLanguage = configuredLocale ? findMinecraftLanguage(configuredLocale) : undefined;
      const targetLocale = configuredLanguage?.code ?? detectTargetLocale(systemLocale);
      // Onboarding中は翻訳先が未確定なので、OS由来の暫定値を設定へ書き戻さない。
      const shouldPersistTargetLocale = !configuredLanguage && settings["onboarding.completed"] !== false;
      loadedSettings["language.uiLanguage"] = uiLanguage;
      loadedSettings["language.targetLocale"] = targetLocale;
      if (!isAppearanceTheme(loadedSettings["appearance.theme"])) {
        loadedSettings["appearance.theme"] = "system";
      }
      cacheAppearanceTheme(loadedSettings["appearance.theme"]);
      setUiLanguage(uiLanguage);
      set({ settings: loadedSettings, savedValues: settings, loaded: true, error: null, systemLocale });
      if (shouldPersistTargetLocale) {
        try {
          // Project作成時も画面と同じ補正済み翻訳先を使えるよう、設定を一度同期する。
          await saveSettings({ "language.targetLocale": targetLocale });
          set((state) => ({
            savedValues: { ...state.savedValues, "language.targetLocale": targetLocale },
          }));
        } catch {
          // 読み込んだ画面の値は維持し、設定の保存失敗で起動を止めない。
        }
      }
    } catch {
      let systemLocale: string | null = null;
      try {
        systemLocale = await getSystemLocale();
      } catch {
        // settings取得の失敗表示は維持し、ロケールが取れない場合は英語を既定値にする。
      }
      setUiLanguage(detectUiLanguage(systemLocale));
      set({ loaded: true, error: t("app.settingsLoadError"), systemLocale });
    }
  },
  save: async (patch) => {
    if (isAppearanceTheme(patch["appearance.theme"])) {
      const previousTheme = get().settings["appearance.theme"];
      const nextTheme = patch["appearance.theme"];
      // テーマは選択直後に反映できるよう、DB応答を待つ間は画面と初期描画cacheを先行更新する。
      set((state) => ({
        settings: { ...state.settings, ...patch },
        savedValues: { ...state.savedValues, ...patch },
        error: null,
      }));
      cacheAppearanceTheme(nextTheme);
      try {
        await saveSettings(patch);
        if (isUiLanguage(patch["language.uiLanguage"])) setUiLanguage(patch["language.uiLanguage"]);
      } catch (error) {
        set((state) => state.settings["appearance.theme"] === nextTheme
          ? {
            settings: { ...state.settings, "appearance.theme": previousTheme },
            savedValues: { ...state.savedValues, "appearance.theme": previousTheme },
          }
          : {});
        if (get().settings["appearance.theme"] === previousTheme) {
          cacheAppearanceTheme(previousTheme);
        }
        throw error;
      }
      return;
    }

    await saveSettings(patch);
    set((state) => ({
      settings: { ...state.settings, ...patch },
      savedValues: { ...state.savedValues, ...patch },
      error: null,
    }));
    if (isUiLanguage(patch["language.uiLanguage"])) setUiLanguage(patch["language.uiLanguage"]);
  },
  value: <T,>(key: string) => {
    const value = get().settings[key];
    return (value ?? SETTING_DEFAULTS[key]) as T;
  },
}));
