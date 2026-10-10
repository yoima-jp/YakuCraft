use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;

/// アセットを提供するレイヤーの種類。Mod、Minecraft本体、Builtinの順で探索する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Mod,
    Minecraft,
    Builtin,
}

/// JARや仮想Builtinなど、パス一覧と必要時の読み込みだけを提供する境界。
pub trait AssetSource: Send + Sync {
    fn paths(&self) -> Vec<String>;
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String>;
}

/// アセットを提供する一つの順序付きレイヤー。
#[derive(Clone)]
pub struct AssetPack {
    pub id: String,
    pub kind: SourceKind,
    pub source: Arc<dyn AssetSource>,
}

impl AssetPack {
    pub fn new(id: impl Into<String>, kind: SourceKind, source: Arc<dyn AssetSource>) -> Self {
        Self {
            id: id.into(),
            kind,
            source,
        }
    }
}

/// 読み込みに失敗したアセットソース。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("asset source {source_id} could not read {path}: {message}")]
pub struct SourceReadError {
    pub source_id: String,
    pub path: String,
    pub message: String,
}
