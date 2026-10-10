use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::{
    ir::{AssetLink, AssetLocation, LinkConfidence},
    source::{AssetPack, SourceKind},
};

/// 翻訳キーに結び付けるアセットの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    Blockstate,
    BlockModel,
    ItemModel,
    ItemDefinition,
    TextureBlock,
    TextureItem,
    TextureEntity,
    GeoModel,
    Animation,
    OtherModel,
}

/// Indexに保存するのはArchive内の場所と出所だけで、画像バイト列は読み込まない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPath {
    pub namespace: String,
    pub asset_type: AssetType,
    pub resource_id: String,
    pub archive_path: String,
    pub source_id: String,
    pub source_kind: SourceKind,
}

/// ソースのパス一覧だけから構築する、決定的なアセット索引。
#[derive(Debug, Clone, Default)]
pub struct AssetIndex {
    assets: Vec<AssetPath>,
    by_archive_path: BTreeMap<(String, String), usize>,
    custom_textures: Vec<CustomTexturePath>,
    custom_textures_by_archive_path: BTreeMap<(String, String), usize>,
    by_namespace: HashMap<String, HashMap<AssetType, AssetTypeIndex>>,
}

#[derive(Debug, Clone, Default)]
struct AssetTypeIndex {
    by_resource_id: HashMap<String, Vec<usize>>,
    by_name: HashMap<String, Vec<usize>>,
    by_token: HashMap<String, Vec<usize>>,
}

#[derive(Debug, Clone)]
// Custom texture paths stay outside the public asset taxonomy; model resolution needs only this
// exact-path metadata, and including them in name/token maps would add unused duplicate indexes.
pub(crate) struct CustomTexturePath {
    pub namespace: String,
    pub resource_id: String,
    pub archive_path: String,
    pub source_id: String,
    pub source_kind: SourceKind,
}

impl AssetIndex {
    pub fn build(packs: &[AssetPack]) -> Self {
        let mut assets = Vec::new();
        let mut custom_textures = Vec::new();
        for pack in packs {
            for archive_path in pack.source.paths() {
                if let Some((namespace, asset_type, resource_id)) = classify_path(&archive_path) {
                    assets.push(AssetPath {
                        namespace,
                        asset_type,
                        resource_id,
                        archive_path,
                        source_id: pack.id.clone(),
                        source_kind: pack.kind,
                    });
                } else if let Some((namespace, resource_id)) =
                    classify_custom_texture_path(&archive_path)
                {
                    custom_textures.push(CustomTexturePath {
                        namespace,
                        resource_id,
                        archive_path,
                        source_id: pack.id.clone(),
                        source_kind: pack.kind,
                    });
                }
            }
        }
        assets.sort_by(|left, right| {
            (&left.source_id, &left.archive_path).cmp(&(&right.source_id, &right.archive_path))
        });
        custom_textures.sort_by(|left, right| {
            (&left.source_id, &left.archive_path).cmp(&(&right.source_id, &right.archive_path))
        });

        let mut by_archive_path = BTreeMap::new();
        let mut custom_textures_by_archive_path = BTreeMap::new();
        let mut by_namespace = HashMap::<String, HashMap<AssetType, AssetTypeIndex>>::new();
        for (index, asset) in assets.iter().enumerate() {
            by_archive_path.insert((asset.source_id.clone(), asset.archive_path.clone()), index);
            let type_index = by_namespace
                .entry(asset.namespace.clone())
                .or_default()
                .entry(asset.asset_type)
                .or_default();
            type_index
                .by_resource_id
                .entry(asset.resource_id.clone())
                .or_default()
                .push(index);
            let name = asset.resource_id.rsplit('/').next().unwrap_or_default();
            type_index
                .by_name
                .entry(name.to_owned())
                .or_default()
                .push(index);
            let tokens: BTreeSet<_> = name
                .split(['_', '-'])
                .filter(|part| part.len() > 1)
                .collect();
            for token in tokens {
                type_index
                    .by_token
                    .entry(token.to_owned())
                    .or_default()
                    .push(index);
            }
        }
        for (index, texture) in custom_textures.iter().enumerate() {
            custom_textures_by_archive_path.insert(
                (texture.source_id.clone(), texture.archive_path.clone()),
                index,
            );
        }
        Self {
            assets,
            by_archive_path,
            custom_textures,
            custom_textures_by_archive_path,
            by_namespace,
        }
    }

    pub fn assets(&self) -> &[AssetPath] {
        &self.assets
    }

    pub fn find<'a>(&'a self, source_id: &str, archive_path: &str) -> Option<&'a AssetPath> {
        self.by_archive_path
            .get(&(source_id.to_owned(), archive_path.to_owned()))
            .and_then(|index| self.assets.get(*index))
    }

    pub(crate) fn find_custom_texture<'a>(
        &'a self,
        source_id: &str,
        archive_path: &str,
    ) -> Option<&'a CustomTexturePath> {
        self.custom_textures_by_archive_path
            .get(&(source_id.to_owned(), archive_path.to_owned()))
            .and_then(|index| self.custom_textures.get(*index))
    }

    pub fn in_namespace(&self, namespace: &str) -> impl Iterator<Item = &AssetPath> {
        self.assets
            .iter()
            .filter(move |asset| asset.namespace == namespace)
    }

    /// 翻訳キーに対するアセット候補を、名前と単語の索引から取り出す。
    ///
    /// ProjectのReview保存では多数のキーについて同じ名前空間を探すため、全Assetを
    /// キーごとに走査するとキー数×Asset数の照合になる。ここでは候補の照合規則を保ち、
    /// 該当する名前だけを索引から集める。
    pub fn links_for_translation_key(&self, key: &str) -> Vec<AssetLink> {
        let mut parts = key.splitn(3, '.');
        let Some(prefix) = parts.next() else {
            return Vec::new();
        };
        let Some(namespace) = parts.next().filter(|namespace| !namespace.is_empty()) else {
            return Vec::new();
        };
        let Some(resource_id) = parts.next().filter(|resource_id| !resource_id.is_empty()) else {
            return Vec::new();
        };
        let expected_types: Vec<(AssetType, String, String)> = match prefix {
            "block" => vec![
                (
                    AssetType::Blockstate,
                    resource_id.to_owned(),
                    format!("assets/{namespace}/blockstates/{resource_id}.json"),
                ),
                (
                    AssetType::BlockModel,
                    format!("block/{resource_id}"),
                    format!("assets/{namespace}/models/block/{resource_id}.json"),
                ),
                (
                    AssetType::TextureBlock,
                    resource_id.to_owned(),
                    format!("assets/{namespace}/textures/block/{resource_id}.png"),
                ),
            ],
            "item" => vec![
                (
                    AssetType::ItemModel,
                    format!("item/{resource_id}"),
                    format!("assets/{namespace}/models/item/{resource_id}.json"),
                ),
                (
                    AssetType::ItemDefinition,
                    resource_id.to_owned(),
                    format!("assets/{namespace}/items/{resource_id}.json"),
                ),
                (
                    AssetType::TextureItem,
                    resource_id.to_owned(),
                    format!("assets/{namespace}/textures/item/{resource_id}.png"),
                ),
            ],
            "entity" => vec![
                (
                    AssetType::TextureEntity,
                    resource_id.to_owned(),
                    format!("assets/{namespace}/textures/entity/{resource_id}.png"),
                ),
                (
                    AssetType::GeoModel,
                    resource_id.to_owned(),
                    format!("assets/{namespace}/geo/{resource_id}.geo.json"),
                ),
            ],
            _ => return Vec::new(),
        };

        let name = resource_id.rsplit('/').next().unwrap_or_default();
        let tokens: BTreeSet<_> = name
            .split(['_', '-'])
            .filter(|part| part.len() > 1)
            .collect();
        let Some(namespace_index) = self.by_namespace.get(namespace) else {
            return Vec::new();
        };
        let mut candidates = HashSet::new();
        for (asset_type, expected_resource_id, _) in &expected_types {
            let Some(type_index) = namespace_index.get(asset_type) else {
                continue;
            };
            if let Some(indexes) = type_index.by_resource_id.get(expected_resource_id) {
                candidates.extend(indexes.iter().copied());
            }
            if let Some(indexes) = type_index.by_name.get(name) {
                candidates.extend(indexes.iter().copied());
            }
            for token in &tokens {
                if let Some(indexes) = type_index.by_token.get(*token) {
                    candidates.extend(indexes.iter().copied());
                }
            }
        }

        let mut candidate_indexes: Vec<_> = candidates.into_iter().collect();
        candidate_indexes.sort_unstable();
        let mut links = Vec::with_capacity(candidate_indexes.len());
        for index in candidate_indexes {
            let asset = &self.assets[index];
            let confidence =
                translation_key_candidate_confidence(asset, name, &tokens, &expected_types);
            let Some(confidence) = confidence else {
                continue;
            };
            links.push(AssetLink {
                asset: AssetLocation::from(asset),
                confidence,
                confirmed: confidence != LinkConfidence::Low,
            });
        }
        // Asset列のsource IDとpath順は、ProjectのRoot・Nested pack順と一致する。
        // 同じ確度ならこの順を保ち、従来のResolverと同じprimary候補を選ぶ。
        links.sort_by_key(|link| match link.confidence {
            LinkConfidence::High => 0,
            LinkConfidence::Medium => 1,
            LinkConfidence::Low => 2,
        });
        links
    }
}

fn translation_key_candidate_confidence(
    asset: &AssetPath,
    wanted_name: &str,
    wanted_tokens: &BTreeSet<&str>,
    expected_types: &[(AssetType, String, String)],
) -> Option<LinkConfidence> {
    let expected_path = expected_types
        .iter()
        .find(|(asset_type, _, _)| *asset_type == asset.asset_type)
        .map(|(_, _, expected_path)| expected_path)?;
    if asset.archive_path == *expected_path {
        return Some(LinkConfidence::High);
    }
    let candidate_name = asset.resource_id.rsplit('/').next()?;
    if candidate_name == wanted_name {
        return Some(LinkConfidence::Medium);
    }
    let has_shared_token = candidate_name
        .split(['_', '-'])
        .filter(|part| part.len() > 1)
        .any(|token| wanted_tokens.contains(token));
    (wanted_tokens.len() > 1 && has_shared_token).then_some(LinkConfidence::Low)
}

fn classify_path(path: &str) -> Option<(String, AssetType, String)> {
    let mut parts = path.split('/');
    if parts.next()? != "assets" {
        return None;
    }
    let namespace = parts.next()?;
    if namespace.is_empty() || namespace == "." || namespace == ".." {
        return None;
    }
    let category = parts.next()?;
    let rest = parts.collect::<Vec<_>>().join("/");
    let (asset_type, resource_id) = match category {
        "blockstates" if rest.ends_with(".json") => {
            (AssetType::Blockstate, rest.trim_end_matches(".json"))
        }
        "models" if rest.ends_with(".json") => {
            let (subfolder, _) = rest.split_once('/').unwrap_or(("", &rest));
            let asset_type = match subfolder {
                "block" => AssetType::BlockModel,
                "item" => AssetType::ItemModel,
                _ => AssetType::OtherModel,
            };
            (asset_type, rest.strip_suffix(".json")?)
        }
        "items" if rest.ends_with(".json") => {
            (AssetType::ItemDefinition, rest.trim_end_matches(".json"))
        }
        "textures" if rest.ends_with(".png") => {
            let (subfolder, _) = rest.split_once('/').unwrap_or(("", &rest));
            let asset_type = match subfolder {
                "block" => AssetType::TextureBlock,
                "item" => AssetType::TextureItem,
                "entity" => AssetType::TextureEntity,
                _ => return None,
            };
            (asset_type, rest.strip_suffix(".png")?)
        }
        // `.geo`/`.animation`は形式固有の拡張子であり、resource IDには含めない。
        "geo" if rest.ends_with(".geo.json") => {
            (AssetType::GeoModel, rest.strip_suffix(".geo.json")?)
        }
        "geo" if rest.ends_with(".json") => (AssetType::GeoModel, rest.strip_suffix(".json")?),
        "animations" if rest.ends_with(".animation.json") => {
            (AssetType::Animation, rest.strip_suffix(".animation.json")?)
        }
        "animations" if rest.ends_with(".json") => {
            (AssetType::Animation, rest.strip_suffix(".json")?)
        }
        _ => return None,
    };
    if resource_id.is_empty()
        || resource_id
            .split('/')
            .any(|part| part == ".." || part.is_empty())
    {
        return None;
    }
    Some((namespace.to_owned(), asset_type, resource_id.to_owned()))
}

fn classify_custom_texture_path(path: &str) -> Option<(String, String)> {
    // Vanilla narrowed automatic atlas inclusion in 1.19.3 for performance, then documented
    // custom texture directories and prefixes. YakuCraft resolves referenced PNGs by exact path,
    // so retain those paths as lightweight metadata without decoding image bytes.
    // https://feedback.minecraft.net/hc/en-us/articles/11280166737293-Minecraft-Java-Edition-1-19-3
    // https://feedback.minecraft.net/hc/en-us/articles/10722895337229-Minecraft-Java-Edition-Snapshot-22w46a
    let mut parts = path.split('/');
    if parts.next()? != "assets" {
        return None;
    }
    let namespace = parts.next()?;
    if namespace.is_empty() || namespace == "." || namespace == ".." || parts.next()? != "textures"
    {
        return None;
    }
    let rest = parts.collect::<Vec<_>>().join("/");
    if !rest.ends_with(".png") {
        return None;
    }
    let (subfolder, _) = rest.split_once('/').unwrap_or(("", &rest));
    if matches!(subfolder, "block" | "item" | "entity") {
        return None;
    }
    let resource_id = rest.strip_suffix(".png")?;
    if resource_id.is_empty()
        || resource_id
            .split('/')
            .any(|part| part == ".." || part.is_empty())
    {
        return None;
    }
    Some((namespace.to_owned(), resource_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{AssetIndex, AssetType};
    use crate::source::{AssetPack, AssetSource, SourceKind};

    struct Paths(Vec<String>);

    impl AssetSource for Paths {
        fn paths(&self) -> Vec<String> {
            self.0.clone()
        }

        fn read(&self, _path: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
    }

    #[test]
    fn classifies_supported_paths_without_reading_texture_bytes() {
        let pack = AssetPack::new(
            "fixture",
            SourceKind::Mod,
            Arc::new(Paths(vec![
                "assets/example/blockstates/machine.json".to_owned(),
                "assets/example/models/block/machine.json".to_owned(),
                "assets/example/models/custom/model.json".to_owned(),
                "assets/example/items/machine.json".to_owned(),
                "assets/example/textures/block/machine.png".to_owned(),
                "assets/example/textures/custom/token.png".to_owned(),
                "assets/example/geo/machine.geo.json".to_owned(),
                "assets/example/animations/machine.animation.json".to_owned(),
            ])),
        );
        let index = AssetIndex::build(&[pack]);
        let types: Vec<_> = index
            .assets()
            .iter()
            .map(|asset| asset.asset_type)
            .collect();
        assert_eq!(
            types,
            vec![
                AssetType::Animation,
                AssetType::Blockstate,
                AssetType::GeoModel,
                AssetType::ItemDefinition,
                AssetType::BlockModel,
                AssetType::OtherModel,
                AssetType::TextureBlock,
            ]
        );
        assert!(
            index
                .find_custom_texture("fixture", "assets/example/textures/custom/token.png")
                .is_some()
        );
        let geo = index
            .assets()
            .iter()
            .find(|asset| asset.asset_type == AssetType::GeoModel)
            .unwrap();
        assert_eq!(geo.resource_id, "machine");
        assert_eq!(geo.archive_path, "assets/example/geo/machine.geo.json");
        let animation = index
            .assets()
            .iter()
            .find(|asset| asset.asset_type == AssetType::Animation)
            .unwrap();
        assert_eq!(animation.resource_id, "machine");
    }
}
