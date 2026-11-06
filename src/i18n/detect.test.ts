import { describe, expect, it } from "vitest";

import { autoTargetLocale, detectTargetLocale, detectUiLanguage, parseOsLocale } from "./detect";

describe("parseOsLocale", () => {
  it.each([
    ["ja_JP.UTF-8", "ja", "JP"],
    ["de_DE@euro", "de", "DE"],
    ["zh-Hant-HK", "zh", "HK"],
    ["sr-Latn-RS", "sr", "RS"],
  ])("POSIX/BCP-47形式の %s を解析する", (input, language, region) => {
    const locale = parseOsLocale(input);
    expect(locale?.language).toBe(language);
    expect(locale?.region).toBe(region);
  });

  it.each(["C", "POSIX"])("%s を英語として扱う", (locale) => {
    expect(parseOsLocale(locale)?.language).toBe("en");
    expect(detectUiLanguage(locale)).toBe("en");
  });

  it("不正な値では例外を出さない", () => {
    expect(parseOsLocale("%%%invalid")).toBeNull();
    expect(detectUiLanguage("%%%invalid")).toBe("en");
    expect(detectTargetLocale("%%%invalid")).toBe("en_us");
  });
});

describe("detectUiLanguage", () => {
  it.each([
    ["ja", "ja"],
    ["ko-KR", "ko"],
    ["en-GB", "en"],
    ["zh-Hans", "zh-Hans"],
    ["zh-Hans-HK", "zh-Hans"],
    ["zh-CN", "zh-Hans"],
    ["zh-SG", "zh-Hans"],
    ["zh-MY", "zh-Hans"],
    ["zh-Hant", "zh-Hant"],
    ["zh-Hant-CN", "zh-Hant"],
    ["zh-TW", "zh-Hant"],
    ["zh-HK", "zh-Hant"],
    ["zh-MO", "zh-Hant"],
    ["fr-FR", "en"],
  ])("%s を %s に対応付ける", (input, expected) => {
    expect(detectUiLanguage(input)).toBe(expected);
  });
});

describe("detectTargetLocale", () => {
  it.each([
    ["ja", "ja_jp"],
    ["de-AT", "de_at"],
    ["de-CH", "de_ch"],
    ["es-MX", "es_mx"],
    ["es-419", "es_mx"],
    ["es-CO", "es_mx"],
    ["es-BZ", "es_mx"],
    ["es-BR", "es_mx"],
    ["en-GB", "en_gb"],
    ["pt-BR", "pt_br"],
    ["pt-PT", "pt_pt"],
    ["fr-CA", "fr_ca"],
    ["nb-NO", "no_no"],
    ["nn-NO", "nn_no"],
    ["sr-Latn-RS", "sr_cs"],
    ["sr-Cyrl-RS", "sr_sp"],
    ["zh-Hans", "zh_cn"],
    ["zh-CN", "zh_cn"],
    ["zh-SG", "zh_cn"],
    ["zh-MY", "zh_cn"],
    ["zh-Hant-HK", "zh_hk"],
    ["zh-MO", "zh_hk"],
    ["zh-TW", "zh_tw"],
    ["zh", "zh_cn"],
    ["tok", "tok"],
    ["sw", "en_us"],
  ])("%s の翻訳先は %s", (input, expected) => {
    expect(detectTargetLocale(input)).toBe(expected);
  });

  it("別名、アラビア語、ジョーク言語を処理する", () => {
    expect(detectTargetLocale("iw-IL")).toBe("he_il");
    expect(detectTargetLocale("in-ID")).toBe("id_id");
    expect(detectTargetLocale("ji-DE")).toBe("yi_de");
    expect(detectTargetLocale("ar-MA")).toBe("ar_sa");
    expect(detectTargetLocale("en-UD")).toBe("en_us");
    expect(detectTargetLocale("enp")).toBe("en_us");
  });
});

describe("autoTargetLocale", () => {
  it("同じ系統のOS言語では地域を反映する", () => {
    expect(autoTargetLocale("zh-Hant", "zh-Hant-HK")).toBe("zh_hk");
    expect(autoTargetLocale("ko", "ko-KR")).toBe("ko_kr");
  });

  it("別系統のOS言語では選択したUI言語の既定値を使う", () => {
    expect(autoTargetLocale("zh-Hans", "en-US")).toBe("zh_cn");
    expect(autoTargetLocale("zh-Hant", "en-US")).toBe("zh_tw");
    expect(autoTargetLocale("ja", "en-US")).toBe("ja_jp");
  });
});
