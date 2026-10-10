use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::index::AssetType;
use crate::source::SourceKind;

pub const RESOLVER_VERSION: u32 = 8;

/// IRやBuiltin geometryを変更した場合はこの値を上げ、古いPreview cacheを無効にする。
pub const fn resolver_version() -> u32 {
    RESOLVER_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetLocation {
    pub source_id: String,
    pub source_kind: SourceKind,
    pub archive_path: String,
    pub namespace: String,
    pub resource_id: String,
    pub asset_type: AssetType,
}

impl From<&crate::index::AssetPath> for AssetLocation {
    fn from(asset: &crate::index::AssetPath) -> Self {
        Self {
            source_id: asset.source_id.clone(),
            source_kind: asset.source_kind,
            archive_path: asset.archive_path.clone(),
            namespace: asset.namespace.clone(),
            resource_id: asset.resource_id.clone(),
            asset_type: asset.asset_type,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetLink {
    pub asset: AssetLocation,
    pub confidence: LinkConfidence,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaceName {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementRotation {
    pub origin: [f64; 3],
    pub axis: FaceAxis,
    pub angle: f64,
    pub rescale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaceAxis {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedElement {
    pub from: [f64; 3],
    pub to: [f64; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<ElementRotation>,
    pub shade: bool,
    pub faces: BTreeMap<FaceName, ResolvedFace>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedFace {
    pub texture: String,
    pub texture_path: Option<AssetLocation>,
    pub uv: [f64; 4],
    pub rotation: u16,
    pub cullface: Option<FaceName>,
    pub tintindex: Option<i32>,
    pub missing: bool,
    pub searched_sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayTransform {
    pub rotation: [f64; 3],
    pub translation: [f64; 3],
    pub scale: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureReference {
    pub location: String,
    pub path: Option<AssetLocation>,
    pub missing: bool,
    pub searched_sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedModel {
    pub model: String,
    pub source: Option<AssetLocation>,
    /// 描画対象のJSONからparentまで、モデル表示と許可済み原文取得に使う順で保持する。
    pub source_chain: Vec<AssetLocation>,
    pub textures: BTreeMap<String, TextureReference>,
    pub elements: Vec<ResolvedElement>,
    pub ambient_occlusion: bool,
    pub display: BTreeMap<String, DisplayTransform>,
    pub gui_light: Option<String>,
    pub generated_item: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unsupported_features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReference {
    pub model: String,
    pub x: i32,
    pub y: i32,
    pub uvlock: bool,
    pub weight: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockstateVariant {
    pub id: String,
    pub selector: String,
    pub multipart_condition: Option<Value>,
    pub models: Vec<ModelReference>,
    pub selected_model: ModelReference,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedBlockModel {
    pub model: ResolvedModel,
    pub x_rotation: i32,
    pub y_rotation: i32,
    pub uvlock: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockPreviewManifest {
    pub translation_key: String,
    pub variants: Vec<BlockstateVariant>,
    pub selected_variant: Option<String>,
    /// Multipartの条件から抽出したpropertyと、現在のpreview state。
    pub properties: Vec<BlockstateProperty>,
    pub selected_state: BTreeMap<String, String>,
    pub models: Vec<ResolvedBlockModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockstateProperty {
    pub name: String,
    pub values: Vec<String>,
    pub selected_value: String,
}

/// Previewの分岐を再解決するときにUIから渡す選択状態。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PreviewOptions {
    pub selected_variant: Option<String>,
    pub blockstate: BTreeMap<String, String>,
    pub item: BTreeMap<String, String>,
    pub selected_texture: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemLayer {
    pub index: u32,
    pub texture: TextureReference,
    /// レイヤーを板として描くときの厚み（Minecraftの16px座標系）。
    pub thickness: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPreviewManifest {
    pub translation_key: String,
    pub model: ResolvedModel,
    pub models: Vec<ResolvedModel>,
    pub layers: Vec<ItemLayer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<ResolvedItemDefinition>,
    pub variants: Vec<ItemDefinitionVariant>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolvedItemDefinition {
    Model {
        model_index: usize,
        tints: Vec<Value>,
    },
    Composite {
        children: Vec<ResolvedItemDefinition>,
    },
    Condition {
        property: String,
        selector: String,
        selected: String,
        child: Box<ResolvedItemDefinition>,
    },
    Select {
        property: String,
        selector: String,
        selected: String,
        child: Box<ResolvedItemDefinition>,
    },
    RangeDispatch {
        property: String,
        selector: String,
        selected: String,
        child: Box<ResolvedItemDefinition>,
    },
    Empty,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDefinitionVariant {
    pub selector: String,
    pub property: String,
    pub selected: String,
    pub options: Vec<ItemDefinitionOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDefinitionOption {
    pub id: String,
    pub value: Option<Value>,
    pub fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityPreviewManifest {
    pub translation_key: String,
    pub geometry: EntityGeometry,
    pub geometry_source: AssetLocation,
    pub confidence: LinkConfidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture: Option<TextureReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture_confidence: Option<LinkConfidence>,
    pub texture_candidates: Vec<TextureCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityGeometry {
    pub identifier: String,
    pub format_version: String,
    pub texture_width: Option<u32>,
    pub texture_height: Option<u32>,
    pub bones: Vec<EntityBone>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityBone {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pivot: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f64; 3]>,
    pub cubes: Vec<EntityCube>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityCube {
    pub origin: [f64; 3],
    pub size: [f64; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pivot: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv: Option<EntityCubeUv>,
    pub inflate: f64,
    pub mirror: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum EntityCubeUv {
    Box([f64; 2]),
    PerFace(BTreeMap<FaceName, EntityFaceUv>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityFaceUv {
    pub uv: [f64; 2],
    pub uv_size: [f64; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureCandidate {
    pub asset: AssetLocation,
    pub confidence: LinkConfidence,
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TexturePreviewManifest {
    pub translation_key: String,
    pub textures: Vec<TextureReference>,
    pub candidates: Vec<TextureCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub raw_model_info: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoPreviewManifest {
    pub translation_key: String,
    pub reason: String,
    pub texture_candidates: Vec<TextureCandidate>,
    pub raw_model_info: Option<Value>,
    pub raw_model_path: Option<String>,
}

/// Three.js向けのPreview DTO。Minecraft形式の解釈はRust側で完了させる。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "manifest", rename_all = "camelCase")]
pub enum PreviewManifest {
    Block(BlockPreviewManifest),
    Item(ItemPreviewManifest),
    Entity(EntityPreviewManifest),
    Texture(TexturePreviewManifest),
    NoPreview(NoPreviewManifest),
}
