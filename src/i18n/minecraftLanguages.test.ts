import { afterEach, describe, expect, it } from "vitest";

import { setUiLanguage } from ".";
import {
  findMinecraftLanguage,
  isBidirectionalLocale,
  minecraftLanguageLabel,
  minecraftLocaleToBcp47,
  MINECRAFT_LANGUAGES,
  targetLanguageName,
} from "./minecraftLanguages";

describe("Minecraft language helpers", () => {
  afterEach(() => setUiLanguage("ja"));

  it("一覧の143件を検索でき、大文字小文字を区別しない", () => {
    expect(MINECRAFT_LANGUAGES).toHaveLength(143);
    expect(findMinecraftLanguage("DE_DE")?.name).toBe("Deutsch");
    expect(minecraftLanguageLabel("unknown_locale")).toBe("unknown_locale");
  });

  it("現在のUI言語で訳名を表示し、人工言語はネイティブ名に戻す", () => {
    expect(targetLanguageName("ja_jp")).toBe("日本語");
    expect(targetLanguageName("de_de")).toBe("ドイツ語");
    expect(targetLanguageName("zh_cn")).toBe("簡体字中国語");
    expect(targetLanguageName("zh_tw")).toBe("繁体字中国語");
    expect(targetLanguageName("zh_hk")).toBe("繁体字中国語");

    setUiLanguage("en");
    expect(targetLanguageName("de_de")).toBe("German");
  });

  it("BCP-47と双方向表記の情報を返す", () => {
    expect(minecraftLocaleToBcp47("zh_cn")).toBe("zh-CN");
    expect(minecraftLocaleToBcp47("zlm_arab")).toBe("zlm-Arab");
    expect(minecraftLocaleToBcp47("bad%%%locale")).toBeUndefined();
    expect(isBidirectionalLocale("AR_SA")).toBe(true);
    expect(isBidirectionalLocale("de_de")).toBe(false);
  });
});
