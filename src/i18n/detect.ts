import { findMinecraftLanguage, MINECRAFT_LANGUAGES } from "./minecraftLanguages";
import type { UiLanguage } from "./uiLanguages";

const jokeLocales = new Set(["en_ud", "en_pt", "enp", "enws", "lol_us"]);
const latinAmericaRegions = new Set([
  "419", "AG", "AI", "AR", "AW", "BB", "BL", "BM", "BO", "BQ", "BR", "BS", "BZ", "CL", "CO", "CR", "CU", "CW", "DM", "DO", "EC", "GD", "GF", "GP", "GT", "GY", "HN", "HT", "JM", "KN", "KY", "LC", "MF", "MQ", "MS", "MX", "NI", "PA", "PE", "PR", "PY", "SR", "SX", "TC", "TT", "UY", "VC", "VE", "VG", "VI",
]);

export function parseOsLocale(raw: string | null | undefined): Intl.Locale | null {
  if (typeof raw !== "string" || raw.trim() === "") return null;
  const withoutEncoding = raw.trim().split(/[.@]/, 1)[0].replaceAll("_", "-");
  if (/^(c|posix)$/i.test(withoutEncoding)) return new Intl.Locale("en-US");

  try {
    return new Intl.Locale(withoutEncoding);
  } catch {
    return null;
  }
}

export function detectUiLanguage(osLocale: string | null | undefined): UiLanguage {
  const locale = parseOsLocale(osLocale);
  if (!locale) return "en";

  switch (locale.language.toLowerCase()) {
    case "ja": return "ja";
    case "ko": return "ko";
    case "en": return "en";
    case "zh":
      if (locale.script === "Hans") return "zh-Hans";
      if (locale.script === "Hant") return "zh-Hant";
      if (["TW", "HK", "MO"].includes(locale.region ?? "")) return "zh-Hant";
      return "zh-Hans";
    default: return "en";
  }
}

export function detectTargetLocale(osLocale: string | null | undefined): string {
  const locale = parseOsLocale(osLocale);
  if (!locale) return "en_us";

  const language = locale.language.toLowerCase();
  const region = locale.region?.toUpperCase();
  const script = locale.script;
  const direct = region ? `${language}_${region.toLowerCase()}` : undefined;
  if (direct && !jokeLocales.has(direct) && findMinecraftLanguage(direct)) return direct;

  if (language === "nb" || language === "no") return "no_no";
  if (language === "nn") return "nn_no";
  if (language === "iw") return "he_il";
  if (language === "in") return "id_id";
  if (language === "ji") return "yi_de";
  if (language === "fil") return "fil_ph";
  if (language === "tl") return "tl_ph";
  if (language === "sr") return script === "Latn" ? "sr_cs" : "sr_sp";
  if (language === "be" && script === "Latn") return "be_latn";
  if (language === "zh") {
    if (script === "Hans" || ["CN", "SG", "MY"].includes(region ?? "")) return "zh_cn";
    if (["HK", "MO"].includes(region ?? "")) return "zh_hk";
    if (script === "Hant" || region === "TW") return "zh_tw";
    return "zh_cn";
  }
  if (language === "es" && (region === "419" || (region !== undefined && latinAmericaRegions.has(region)))) {
    return "es_mx";
  }
  if (language === "ar") return "ar_sa";

  const defaults: Partial<Record<string, string>> = {
    en: "en_us",
    es: "es_es",
    pt: "pt_pt",
    fr: "fr_fr",
    de: "de_de",
    nl: "nl_nl",
  };
  if (defaults[language]) return defaults[language];
  if (!jokeLocales.has(language) && findMinecraftLanguage(language)) return language;

  const regionalMatch = MINECRAFT_LANGUAGES.find(({ code }) =>
    code.startsWith(`${language}_`) && !jokeLocales.has(code),
  )?.code;
  return regionalMatch ?? "en_us";
}

export function autoTargetLocale(uiLanguage: UiLanguage, osLocale: string | null | undefined): string {
  if (uiLanguage === "en") return detectTargetLocale(osLocale);
  if (detectUiLanguage(osLocale) === uiLanguage) return detectTargetLocale(osLocale);

  // OSの英語をそのまま翻訳先にすると、選択UIが出ないまま原文と同じ言語になり得る。
  const defaults: Record<Exclude<UiLanguage, "en">, string> = {
    ja: "ja_jp",
    ko: "ko_kr",
    "zh-Hans": "zh_cn",
    "zh-Hant": "zh_tw",
  };
  return defaults[uiLanguage];
}
