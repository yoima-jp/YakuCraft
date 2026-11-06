export const UI_LANGUAGES = ["ja", "en", "zh-Hans", "zh-Hant", "ko"] as const;

export type UiLanguage = (typeof UI_LANGUAGES)[number];

export const UI_LANGUAGE_INFO: Record<UiLanguage, { nativeName: string; intlTag: string }> = {
  ja: { nativeName: "日本語", intlTag: "ja-JP" },
  en: { nativeName: "English", intlTag: "en-US" },
  "zh-Hans": { nativeName: "简体中文", intlTag: "zh-Hans-CN" },
  "zh-Hant": { nativeName: "繁體中文", intlTag: "zh-Hant-TW" },
  ko: { nativeName: "한국어", intlTag: "ko-KR" },
};

export function isUiLanguage(value: unknown): value is UiLanguage {
  return typeof value === "string" && UI_LANGUAGES.some((language) => language === value);
}
