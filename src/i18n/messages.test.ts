import { describe, expect, it } from "vitest";

import { MESSAGE_CATALOG } from "./messages";
import { UI_LANGUAGES } from "./uiLanguages";

const placeholderPattern = /\{([^{}]+)\}/g;
const kanaPattern = /[\u3040-\u30ff\u31f0-\u31ff]/u;

describe("message catalog", () => {
  it("すべての名前空間で5言語のキーとプレースホルダーを揃える", () => {
    expect(Object.keys(MESSAGE_CATALOG).sort()).toEqual([
      "ai", "analyze", "app", "common", "editor", "entryPreview", "export", "glossary",
      "home", "onboarding", "preview", "review", "scan", "settings", "updater",
    ]);

    for (const [namespaceName, namespace] of Object.entries(MESSAGE_CATALOG)) {
      const japanese = namespace.ja as Record<string, string>;
      const expectedKeys = Object.keys(japanese).sort();
      for (const language of UI_LANGUAGES) {
        const translated = namespace[language] as Record<string, string>;
        expect(Object.keys(translated).sort(), `${namespaceName}/${language} keys`).toEqual(expectedKeys);
        for (const [key, value] of Object.entries(translated)) {
          expect(value.trim(), `${namespaceName}/${language}/${key} must not be empty`).not.toBe("");
          expect([...value.matchAll(placeholderPattern)].map((match) => match[1]).sort())
            .toEqual([...japanese[key].matchAll(placeholderPattern)].map((match) => match[1]).sort());
          if (language !== "ja") expect(value, `${namespaceName}/${language}/${key} must not contain kana`).not.toMatch(kanaPattern);
        }
      }
    }
  });
});
