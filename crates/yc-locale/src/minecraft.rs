use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct MinecraftLanguage {
    pub code: String,
    pub name: String,
    pub region: String,
    #[serde(default)]
    pub bidirectional: bool,
}

static MINECRAFT_LANGUAGES: OnceLock<Vec<MinecraftLanguage>> = OnceLock::new();

const MINECRAFT_LANGUAGES_JSON: &str = include_str!("../data/minecraft-languages.json");

pub fn minecraft_languages() -> &'static [MinecraftLanguage] {
    MINECRAFT_LANGUAGES
        .get_or_init(|| {
            serde_json::from_str(MINECRAFT_LANGUAGES_JSON)
                .expect("embedded Minecraft language table must be valid JSON")
        })
        .as_slice()
}

pub fn find_minecraft_language(code: &str) -> Option<&'static MinecraftLanguage> {
    minecraft_languages()
        .iter()
        .find(|language| language.code.eq_ignore_ascii_case(code))
}

pub fn is_constructed_language(code: &str) -> bool {
    let normalized = code.trim().replace('-', "_");
    matches!(
        normalized.to_ascii_lowercase().as_str(),
        "en_ud"
            | "en_pt"
            | "enp"
            | "enws"
            | "lol_us"
            | "qya_aa"
            | "tlh_aa"
            | "tok"
            | "jbo_en"
            | "io_en"
            | "isv"
            | "vp_vl"
    )
}
