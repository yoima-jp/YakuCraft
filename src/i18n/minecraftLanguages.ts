import minecraftLanguageData from "../../crates/yc-locale/data/minecraft-languages.json";

import { getUiLanguage } from "./store";
import { UI_LANGUAGE_INFO } from "./uiLanguages";

export interface MinecraftLanguage {
  code: string;
  name: string;
  region: string;
  bidirectional?: boolean;
}

export const MINECRAFT_LANGUAGES = minecraftLanguageData as MinecraftLanguage[];

const languagesByCode = new Map(MINECRAFT_LANGUAGES.map((language) => [language.code.toLowerCase(), language]));

export function findMinecraftLanguage(code: string): MinecraftLanguage | undefined {
  return languagesByCode.get(code.toLowerCase());
}

export function minecraftLanguageLabel(code: string): string {
  const language = findMinecraftLanguage(code);
  return language ? `${language.name} (${language.region})` : code;
}

export function targetLanguageName(code: string, uiLanguage = getUiLanguage()): string {
  const language = findMinecraftLanguage(code);
  if (!language) return code;

  const normalizedCode = language.code.toLowerCase();
  if (uiLanguage === "ja" && normalizedCode === "zh_cn") return "簡体字中国語";
  if (uiLanguage === "ja" && (normalizedCode === "zh_tw" || normalizedCode === "zh_hk")) return "繁体字中国語";
  const displayCode = normalizedCode === "zh_cn"
    ? "zh-Hans"
    : normalizedCode === "zh_tw" || normalizedCode === "zh_hk"
      ? "zh-Hant"
      : normalizedCode.split("_")[0];

  try {
    const name = new Intl.DisplayNames([UI_LANGUAGE_INFO[uiLanguage].intlTag], { type: "language" })
      .of(displayCode);
    if (name && name.toLowerCase() !== displayCode.toLowerCase()) return name;
  } catch {
    // 人工言語などIntlで解決できないコードは、一覧にあるネイティブ名を使う。
  }
  return language.name;
}

export function minecraftLocaleToBcp47(code: string): string | undefined {
  try {
    return new Intl.Locale(code.replaceAll("_", "-")).toString();
  } catch {
    return undefined;
  }
}

export function isBidirectionalLocale(code: string): boolean {
  return findMinecraftLanguage(code)?.bidirectional === true;
}
