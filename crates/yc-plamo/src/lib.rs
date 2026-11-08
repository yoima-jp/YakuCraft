//! PLaMo 3 Translate をローカルの `llama-server` 経由で使うためのコア。
//!
//! 役割の分担:
//! - [`catalog`][]: 公式 GGUF の固定情報（commit・サイズ・SHA-256・ライセンス）
//! - [`languages`][]: Minecraft ロケールから PLaMo 言語コードへの対応
//! - [`pfml3`][]: PLaMo のプロンプト形式 PFML3 の組み立てと特殊トークン注入の防御
//! - [`protect`][]: placeholder・書式コードのマスク／復元と厳密検証
//! - [`PlamoProvider`][]: `llama-server` の `/completion` を呼ぶ [`yc_ai::TranslationProvider`]
//!
//! モデルのダウンロードとサーバ起動は `yc-llama` の責務で、このクレートは起動済み
//! サーバの base URL と API key だけを受け取る。
//!
//! ```no_run
//! use yc_plamo::{PlamoProvider, PlamoProviderOptions};
//!
//! # fn main() -> Result<(), yc_ai::AiError> {
//! let provider = PlamoProvider::new(
//!     "http://127.0.0.1:9931",
//!     None,
//!     PlamoProviderOptions::default(),
//! )?;
//! # let _ = provider;
//! # Ok(())
//! # }
//! ```

pub mod catalog;
pub mod languages;
pub mod pfml3;
pub mod protect;
pub mod provider;

pub use catalog::{
    DEFAULT_MODEL_ID, LICENSE_ID, LICENSE_URL_EN, LICENSE_URL_JA, PlamoModel, QUANTIZATION,
    default_model, find_model, models,
};
pub use languages::{PLAMO_LANGUAGES, plamo_code_for_locale, supported_target_locales};
pub use pfml3::{Pfml3Error, PromptRequest, build_prompt};
pub use protect::{ProtectError, ProtectStrategy, ProtectedSource};
pub use provider::{
    DEFAULT_MAX_OUTPUT_TOKENS, MIN_OUTPUT_TOKENS, PlamoProvider, PlamoProviderOptions,
    ShaderOptionDetector,
};

#[cfg(test)]
mod tests;
