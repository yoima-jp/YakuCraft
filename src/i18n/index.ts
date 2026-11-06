import { useCallback } from "react";

import type { MessageArguments } from "./define";
import { messages } from "./messages";
import type { JapaneseMessage, MessageKey } from "./messages";
import { getUiLanguage, setUiLanguage, useI18nStore } from "./store";
import { UI_LANGUAGE_INFO } from "./uiLanguages";

export { UI_LANGUAGES, UI_LANGUAGE_INFO, type UiLanguage, isUiLanguage } from "./uiLanguages";
export { setUiLanguage };
export { MESSAGE_CATALOG } from "./messages";

export function t<Key extends MessageKey>(key: Key, ...args: MessageArguments<JapaneseMessage<Key>>): string;
export function t(key: MessageKey, params?: Record<string, string | number>): string {
  return translate(key, params);
}

function translate(key: MessageKey, params?: Record<string, string | number>): string {
  return translateForLanguage(getUiLanguage(), key, params);
}

function translateForLanguage(language: ReturnType<typeof getUiLanguage>, key: MessageKey, params?: Record<string, string | number>): string {
  const paramsValue = params ?? {};
  const count = paramsValue.count;
  const pluralKey = typeof count === "number" && new Intl.PluralRules(UI_LANGUAGE_INFO[language].intlTag).select(count) === "one"
    ? `${key}_one`
    : key;
  const candidates = pluralKey === key ? [key] : [pluralKey, key];
  let message: string | undefined;

  for (const locale of [language, "en", "ja"] as const) {
    const localeMessages = messages[locale] as Record<string, string>;
    message = candidates.map((candidate) => localeMessages[candidate]).find((value) => value !== undefined);
    if (message !== undefined) break;
  }

  return (message ?? key).replace(/\{([^{}]+)\}/g, (placeholder, name: string) =>
    Object.hasOwn(paramsValue, name) ? String(paramsValue[name]) : placeholder,
  );
}

export function useT() {
  const language = useI18nStore((state) => state.uiLanguage);
  return useCallback(<Key extends MessageKey>(key: Key, ...args: MessageArguments<JapaneseMessage<Key>>) => {
    const params = (args as readonly (Record<string, string | number> | undefined)[])[0];
    return translateForLanguage(language, key, params);
  }, [language]);
}

export function useUiLanguage() {
  return useI18nStore((state) => state.uiLanguage);
}

export function formatNumber(value: number): string {
  return new Intl.NumberFormat(UI_LANGUAGE_INFO[getUiLanguage()].intlTag).format(value);
}

export function formatDateTime(value: Date | string | number): string {
  return new Intl.DateTimeFormat(UI_LANGUAGE_INFO[getUiLanguage()].intlTag, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(value instanceof Date ? value : new Date(value));
}

export function compareText(left: string, right: string, language = getUiLanguage()): number {
  return new Intl.Collator(UI_LANGUAGE_INFO[language].intlTag).compare(left, right);
}

export function lowerText(value: string, language = getUiLanguage()): string {
  return value.toLocaleLowerCase(UI_LANGUAGE_INFO[language].intlTag);
}
