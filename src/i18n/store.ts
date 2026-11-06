import { create } from "zustand";

import type { UiLanguage } from "./uiLanguages";

interface I18nState {
  uiLanguage: UiLanguage;
  setUiLanguage: (language: UiLanguage) => void;
}

export const useI18nStore = create<I18nState>()((set) => ({
  uiLanguage: "ja",
  setUiLanguage: (uiLanguage) => set({ uiLanguage }),
}));

export function getUiLanguage() {
  return useI18nStore.getState().uiLanguage;
}

export function setUiLanguage(language: UiLanguage) {
  useI18nStore.getState().setUiLanguage(language);
}
