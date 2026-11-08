//! Minecraft ロケールと PLaMo の言語コードの対応。
//!
//! PLaMo 3 Translate は 53 言語しか扱えず、Minecraft の言語一覧には方言・地域言語・
//! 冗談言語が多数含まれる。翻訳できない対象へ送るとモデルは指定外の言語を出力するため、
//! ここで明示的に落とし、UI 側は [`supported_target_locales`] で選択肢を絞る。

use yc_locale::is_constructed_language;

/// PLaMo 3 Translate が対応する 53 言語のコード。モデルカードの一覧をそのまま持つ。
pub const PLAMO_LANGUAGES: [&str; 53] = [
    "ja", "en", "zh-hans", "zh-hant", "ko", "ar", "az", "be", "bg", "bn", "ca", "ceb", "de", "es",
    "et", "eu", "fa", "fr", "gu", "he", "hi", "hr", "hu", "hy", "id", "it", "jv", "ka", "kk", "km",
    "lt", "lv", "ml", "mr", "ms", "my", "ne", "nl", "pa", "pl", "ru", "si", "sk", "sl", "sr", "sw",
    "ta", "te", "th", "tl", "ur", "uz", "vi",
];

/// Minecraft ロケール固有の読み替え。言語サブタグだけでは決められないものだけを書く。
///
/// - `zh_*` は簡体字／繁体字で別の PLaMo コードになる。
/// - `de_at` / `de_ch` は Minecraft 上でバイエルン系・スイスドイツ語の方言名を持つため、
///   標準ドイツ語の `de` ではなく未対応として扱う。
/// - `val_es` はバレンシア語だが、公式にはカタルーニャ語と同じ言語なので `ca` にする。
/// - `fil_ph`（フィリピノ）と `zlm_arab`（ジャウィ文字のマレー語）は PLaMo 側に対応コードが
///   無く、それぞれ `tl` と `ms` が同じ標準語を指す。
fn override_for_locale(locale: &str) -> Option<Option<&'static str>> {
    Some(match locale {
        "zh_cn" | "zh_hans" => Some("zh-hans"),
        "zh_tw" | "zh_hk" | "zh_hant" => Some("zh-hant"),
        "de_at" | "de_ch" => None,
        "val_es" => Some("ca"),
        "fil_ph" => Some("tl"),
        "zlm_arab" => Some("ms"),
        _ => return None,
    })
}

fn normalize(locale: &str) -> String {
    locale.trim().replace('-', "_").to_ascii_lowercase()
}

/// Minecraft ロケールコードを PLaMo の言語コードへ変換する。未対応なら `None`。
///
/// 地域サブタグは無視し、標準的な書き言葉だけを見る（`en_gb` も `es_mx` も
/// それぞれ `en` / `es`）。方言・歴史的表記・PLaMo 未対応言語は `None`。
pub fn plamo_code_for_locale(locale: &str) -> Option<&'static str> {
    // 冗談言語や人工言語は「似た言語」へ寄せると意味が壊れるため、最初に落とす。
    if is_constructed_language(locale) {
        return None;
    }
    let normalized = normalize(locale);
    if let Some(overridden) = override_for_locale(&normalized) {
        return overridden;
    }

    let language = normalized
        .split_once('_')
        .map_or(normalized.as_str(), |(language, _)| language);
    PLAMO_LANGUAGES
        .iter()
        .copied()
        .find(|code| *code == language)
}

/// 翻訳先として指定できる Minecraft ロケールの一覧。
///
/// `yc_locale` の全言語を走査して対応のあるものだけを返すため、言語表が更新されれば
/// この一覧も自動で追随する。
pub fn supported_target_locales() -> Vec<&'static str> {
    yc_locale::minecraft_languages()
        .iter()
        .map(|language| language.code.as_str())
        .filter(|code| plamo_code_for_locale(code).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_variants_split_into_simplified_and_traditional() {
        assert_eq!(plamo_code_for_locale("zh_cn"), Some("zh-hans"));
        assert_eq!(plamo_code_for_locale("zh_tw"), Some("zh-hant"));
        assert_eq!(plamo_code_for_locale("zh_hk"), Some("zh-hant"));
        // 文言（Classical Chinese）は PLaMo の対象外。
        assert_eq!(plamo_code_for_locale("lzh"), None);
    }

    #[test]
    fn regional_variants_use_the_standard_written_language() {
        assert_eq!(plamo_code_for_locale("en_us"), Some("en"));
        assert_eq!(plamo_code_for_locale("en_gb"), Some("en"));
        assert_eq!(plamo_code_for_locale("es_mx"), Some("es"));
        assert_eq!(plamo_code_for_locale("fr_ca"), Some("fr"));
        assert_eq!(plamo_code_for_locale("nl_be"), Some("nl"));
        assert_eq!(plamo_code_for_locale("ms_my"), Some("ms"));
        assert_eq!(plamo_code_for_locale("fil_ph"), Some("tl"));
        assert_eq!(plamo_code_for_locale("tl_ph"), Some("tl"));
        assert_eq!(plamo_code_for_locale("sr_cs"), Some("sr"));
        assert_eq!(plamo_code_for_locale("sr_sp"), Some("sr"));
        assert_eq!(plamo_code_for_locale("val_es"), Some("ca"));
    }

    #[test]
    fn dialects_and_unsupported_languages_are_rejected() {
        // 方言・地域言語は標準語へ寄せない。語彙が違い、訳文が不自然になる。
        for locale in [
            "de_at", "de_ch", "bar", "nds_de", "esan", "fra_de", "sxu", "szl", "brb", "rpr", "qid",
        ] {
            assert_eq!(plamo_code_for_locale(locale), None, "{locale}");
        }
        // PLaMo が対応しない主要言語。
        for locale in [
            "pt_br", "pt_pt", "tr_tr", "uk_ua", "cs_cz", "da_dk", "sv_se",
        ] {
            assert_eq!(plamo_code_for_locale(locale), None, "{locale}");
        }
        assert_eq!(plamo_code_for_locale("not_a_locale"), None);
        assert_eq!(plamo_code_for_locale(""), None);
    }

    #[test]
    fn constructed_languages_are_rejected_before_any_prefix_matching() {
        // `en_ud` は `en` 系だが英語ではない。接頭辞一致より先に判定する必要がある。
        for locale in [
            "en_ud", "en_pt", "enp", "enws", "lol_us", "tok", "tlh_aa", "qya_aa",
        ] {
            assert_eq!(plamo_code_for_locale(locale), None, "{locale}");
        }
    }

    #[test]
    fn supported_target_locales_are_a_subset_of_the_minecraft_list() {
        let locales = supported_target_locales();
        assert!(!locales.is_empty());
        assert!(locales.len() < yc_locale::minecraft_languages().len());
        assert!(locales.contains(&"ja_jp"));
        assert!(locales.contains(&"en_us"));
        assert!(!locales.contains(&"pt_br"));
        assert!(!locales.contains(&"en_ud"));
        assert!(!locales.contains(&"lzh"));
        for locale in &locales {
            assert!(
                yc_locale::find_minecraft_language(locale).is_some(),
                "{locale} is not a Minecraft locale"
            );
            assert!(plamo_code_for_locale(locale).is_some(), "{locale}");
        }
    }
}
