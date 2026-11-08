//! PLaMo 3 Translate 用のモデルカタログ。
//!
//! 公式 GGUF リポジトリは gated ではないが、`main` の内容は差し替わり得る。
//! ダウンロードは再現可能でなければならないため、URL には必ず commit を固定する。

use std::fmt;

/// ダウンロード対象の量子化。公式の既定は Q4_K_M。
pub const QUANTIZATION: &str = "Q4_K_M";

/// 既定で使うモデルの ID。0.8B は品質が落ち、8B は CPU 実行が現実的でないため 2B を既定にする。
pub const DEFAULT_MODEL_ID: &str = "plamo-3-translate-2b";

/// PLaMo Community License の識別子。SPDX には未登録なので `LicenseRef-` 形式で表す。
pub const LICENSE_ID: &str = "LicenseRef-PLaMo-Community-License";

/// ライセンス本文（日本語）。
pub const LICENSE_URL_JA: &str = "https://plamo.preferredai.jp/info/plamo-community-license-ja";

/// ライセンス本文（英語）。
pub const LICENSE_URL_EN: &str = "https://plamo.preferredai.jp/info/plamo-community-license-en";

/// カタログに載っている 1 モデル分の固定情報。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlamoModel {
    /// アプリ内で使う安定 ID。UI の設定に保存されるため変更しない。
    pub id: &'static str,
    /// 画面表示用のパラメータ数ラベル。
    pub parameters: &'static str,
    /// Hugging Face のリポジトリ名。
    pub repo: &'static str,
    /// 固定した commit。`main` を追うとハッシュ検証が通らなくなる。
    pub commit: &'static str,
    /// リポジトリ内のファイル名。
    pub file: &'static str,
    /// ファイルサイズ（バイト）。ダウンロードの進捗表示と事前の空き容量確認に使う。
    pub size_bytes: u64,
    /// ファイルの SHA-256（16 進小文字）。
    pub sha256: &'static str,
}

impl PlamoModel {
    /// commit を固定したダウンロード URL。Hugging Face は 302 で署名付き CDN URL を返す。
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.commit, self.file
        )
    }

    /// Hugging Face 上のモデルページ。
    pub fn model_page_url(&self) -> String {
        format!("https://huggingface.co/{}", self.repo)
    }
}

impl fmt::Display for PlamoModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.id, self.parameters)
    }
}

/// 公式 GGUF リポジトリの Q4_K_M。値は各モデルカードの記載どおり。
static MODELS: [PlamoModel; 3] = [
    PlamoModel {
        id: "plamo-3-translate-0.8b",
        parameters: "0.8B",
        repo: "pfnet/plamo-3-translate-0.8b-GGUF",
        commit: "e10c327b7a7ce84ffc427cb4b09360b781429fda",
        file: "plamo-3-translate-0.8b-Q4_K_M.gguf",
        size_bytes: 573_126_848,
        sha256: "70891ec70dc7ab4205745a38307867c2881b99239f20327927633abfe4bc9ec6",
    },
    PlamoModel {
        id: "plamo-3-translate-2b",
        parameters: "2B",
        repo: "pfnet/plamo-3-translate-2b-GGUF",
        commit: "228a27cc0bb4f111219963848d50a3127c16ca73",
        file: "plamo-3-translate-2b-Q4_K_M.gguf",
        size_bytes: 1_807_203_040,
        sha256: "1ffd9073afcc70e35ccc942428aa2b73d7aa2b9ea779bf504b775369e0276c13",
    },
    PlamoModel {
        id: "plamo-3-translate-8b",
        parameters: "8B",
        repo: "pfnet/plamo-3-translate-8b-GGUF",
        commit: "fccc6b15efc7d615189fa5df6a1a02ecd2dbc18f",
        file: "plamo-3-translate-8b-Q4_K_M.gguf",
        size_bytes: 5_280_445_728,
        sha256: "94e5d3f91b421f7c8900ddaac27b11a19f7441426c954bbb25a2fd60a1640187",
    },
];

/// カタログ全件を小さい順に返す。
pub fn models() -> &'static [PlamoModel] {
    &MODELS
}

/// ID からモデルを引く。未知の ID は `None`（設定ファイルの破損や旧バージョンの値）。
pub fn find_model(id: &str) -> Option<&'static PlamoModel> {
    MODELS.iter().find(|model| model.id == id)
}

/// 既定モデル。`DEFAULT_MODEL_ID` がカタログに無い状態は実装の誤りなので panic する。
pub fn default_model() -> &'static PlamoModel {
    find_model(DEFAULT_MODEL_ID).expect("the default model id must exist in the catalog")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_are_consistent_and_pinned() {
        assert_eq!(models().len(), 3);
        assert_eq!(default_model().id, "plamo-3-translate-2b");
        for model in models() {
            assert!(find_model(model.id).is_some());
            // commit を固定していない URL は再現性が無いので許可しない。
            assert_eq!(model.commit.len(), 40, "{}", model.id);
            assert!(
                model
                    .download_url()
                    .contains(&format!("/resolve/{}/{}", model.commit, model.file)),
                "{}",
                model.id
            );
            assert_eq!(model.sha256.len(), 64, "{}", model.id);
            assert!(model.file.ends_with(&format!("-{QUANTIZATION}.gguf")));
            assert!(model.size_bytes > 0);
        }
    }

    #[test]
    fn unknown_model_ids_are_rejected() {
        assert!(find_model("plamo-3-translate-31b").is_none());
        assert!(find_model("").is_none());
    }
}
