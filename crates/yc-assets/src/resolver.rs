use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    builtin,
    index::{AssetIndex, AssetType},
    ir::{
        AssetLink, AssetLocation, BlockPreviewManifest, BlockstateProperty, BlockstateVariant,
        EntityBone, EntityCube, EntityCubeUv, EntityFaceUv, EntityGeometry, EntityPreviewManifest,
        ItemDefinitionOption, ItemDefinitionVariant, LinkConfidence, ModelReference,
        NoPreviewManifest, PreviewManifest, PreviewOptions, ResolvedBlockModel,
        ResolvedItemDefinition, ResolvedModel, TextureCandidate, TexturePreviewManifest,
        TextureReference,
    },
    model::{RawDisplayTransform, RawElement, RawFace, RawModel, resolve_element},
    source::{AssetPack, AssetSource, SourceKind, SourceReadError},
};

const MAX_MODEL_DEPTH: usize = 64;

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error(transparent)]
    SourceRead(#[from] SourceReadError),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("model requires an unsupported Java renderer: {0}")]
    UnsupportedModel(String),
    #[error("model JSON is invalid at {path} ({source_id}): {message}")]
    InvalidModelJson {
        source_id: String,
        path: String,
        message: String,
    },
    #[error("model parent cycle: {0:?}")]
    ParentCycle(Vec<String>),
    #[error("model inheritance exceeds the maximum depth of {MAX_MODEL_DEPTH}")]
    MaximumModelDepth,
    #[error("texture variable was not defined: #{0}")]
    MissingTextureVariable(String),
    #[error("texture variable cycle: {0:?}")]
    TextureVariableCycle(Vec<String>),
    #[error("invalid resource location: {0}")]
    InvalidResourceLocation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranslationKind {
    Block,
    Item,
    Entity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TranslationResource {
    kind: TranslationKind,
    namespace: String,
    id: String,
}

#[derive(Debug, Clone)]
struct ModelChain {
    source_chain: Vec<AssetLocation>,
    textures: BTreeMap<String, String>,
    elements: Vec<RawElement>,
    ambient_occlusion: bool,
    display: BTreeMap<String, RawDisplayTransform>,
    gui_light: Option<String>,
    generated_item: bool,
    unsupported_features: Vec<String>,
}

#[derive(Debug, Clone)]
struct RawModelInfo {
    value: Value,
    archive_path: String,
}

/// Minecraft本体JARを任意で含む、順序付きアセット索引とモデル解決器。
pub struct AssetResolver {
    packs: Vec<AssetPack>,
    index: AssetIndex,
}

impl AssetResolver {
    /// Mod packを先に、任意のMinecraft packを次にし、独自Builtin定義を最後に追加する。
    pub fn new(mut packs: Vec<AssetPack>) -> Self {
        packs.sort_by_key(|pack| pack.kind);
        packs.push(AssetPack::new(
            "yc-assets:builtin",
            SourceKind::Builtin,
            Arc::new(BuiltinSource(builtin::models())),
        ));
        let index = AssetIndex::build(&packs);
        Self { packs, index }
    }

    pub fn index(&self) -> &AssetIndex {
        &self.index
    }

    /// Translation keyに対する候補と確度を返す。Low候補はconfirmed=falseのまま残す。
    pub fn links_for_translation_key(&self, translation_key: &str) -> Vec<AssetLink> {
        let Some(resource) = parse_translation_key(translation_key) else {
            return Vec::new();
        };
        let mut links: Vec<_> = self
            .index
            .in_namespace(&resource.namespace)
            .filter_map(|asset| {
                let confidence = candidate_confidence(asset, &resource)?;
                Some(AssetLink {
                    asset: AssetLocation::from(asset),
                    confidence,
                    confirmed: confidence != LinkConfidence::Low,
                })
            })
            .collect();
        links.sort_by(|left: &AssetLink, right: &AssetLink| {
            confidence_rank(left.confidence)
                .cmp(&confidence_rank(right.confidence))
                .then_with(|| {
                    let left_priority = self
                        .packs
                        .iter()
                        .position(|pack| pack.id == left.asset.source_id)
                        .unwrap_or(usize::MAX);
                    let right_priority = self
                        .packs
                        .iter()
                        .position(|pack| pack.id == right.asset.source_id)
                        .unwrap_or(usize::MAX);
                    left_priority
                        .cmp(&right_priority)
                        .then_with(|| left.asset.archive_path.cmp(&right.asset.archive_path))
                })
        });
        links
    }

    /// Model JSONを親から継承し、textures参照とUVを完全解決する。
    pub fn resolve_model(&self, model: &str) -> Result<ResolvedModel, ResolverError> {
        let location = parse_resource_location(model)?;
        let mut stack = Vec::new();
        let mut visited = HashSet::new();
        let (chain, source_asset) =
            self.resolve_model_chain(&location, &mut stack, &mut visited, 0)?;
        let mut textures = BTreeMap::new();
        let mut resolved_texture_variables = HashMap::new();
        for name in chain.textures.keys() {
            let texture_location = self.resolve_texture_variable(
                name,
                &chain.textures,
                &mut resolved_texture_variables,
            )?;
            textures.insert(name.clone(), self.texture_reference(&texture_location)?);
        }

        let mut elements = Vec::with_capacity(chain.elements.len());
        for raw_element in chain.elements {
            let resolved = resolve_element(raw_element, |face_name, face: RawFace, from, to| {
                let texture_location = self.resolve_texture_value(
                    &face.texture,
                    &chain.textures,
                    &mut resolved_texture_variables,
                )?;
                let texture = self.texture_reference(&texture_location)?;
                let uv = face
                    .uv
                    .unwrap_or_else(|| crate::model::default_uv(face_name, from, to));
                Ok::<_, ResolverError>(crate::ir::ResolvedFace {
                    texture: texture_location,
                    texture_path: texture.path,
                    uv,
                    rotation: face.rotation,
                    cullface: face.cullface,
                    tintindex: face.tintindex,
                    missing: texture.missing,
                    searched_sources: texture.searched_sources,
                })
            })?;
            elements.push(resolved);
        }

        Ok(ResolvedModel {
            model: location.to_string(),
            source: Some(source_asset),
            source_chain: chain.source_chain,
            textures,
            elements,
            ambient_occlusion: chain.ambient_occlusion,
            display: chain
                .display
                .into_iter()
                .map(|(context, transform)| (context, transform.into()))
                .collect(),
            gui_light: chain.gui_light,
            generated_item: chain.generated_item,
            unsupported_features: chain.unsupported_features,
        })
    }

    /// Translation keyからBlock、legacy Item、texture fallback用のmanifestを作る。
    pub fn resolve_preview(
        &self,
        translation_key: &str,
        selected_variant: Option<&str>,
    ) -> PreviewManifest {
        let options = PreviewOptions {
            selected_variant: selected_variant.map(str::to_owned),
            ..PreviewOptions::default()
        };
        self.resolve_preview_with_options(translation_key, &options)
    }

    /// UIのVariant Selectorで選んだblockstate/item branchを適用してPreviewを解決する。
    pub fn resolve_preview_with_options(
        &self,
        translation_key: &str,
        options: &PreviewOptions,
    ) -> PreviewManifest {
        let Some(resource) = parse_translation_key(translation_key) else {
            return self.no_preview(translation_key, "unsupported translation key", None);
        };
        match resource.kind {
            TranslationKind::Block => {
                self.resolve_block_preview(translation_key, &resource, options)
            }
            TranslationKind::Item => self.resolve_item_preview(translation_key, &resource, options),
            TranslationKind::Entity => {
                self.resolve_entity_preview(translation_key, &resource, options)
            }
        }
    }

    fn resolve_block_preview(
        &self,
        translation_key: &str,
        resource: &TranslationResource,
        options: &PreviewOptions,
    ) -> PreviewManifest {
        let state_path = format!(
            "assets/{}/blockstates/{}.json",
            resource.namespace, resource.id
        );
        let mut blockstate_bytes = None;
        for pack in &self.packs {
            let Some(asset) = self.index.find(&pack.id, &state_path) else {
                continue;
            };
            match pack.source.read(&asset.archive_path) {
                Ok(Some(bytes)) => {
                    blockstate_bytes = Some((bytes, asset.archive_path.clone()));
                    break;
                }
                Ok(None) => tracing::debug!(
                    source_id = %pack.id,
                    path = %state_path,
                    "indexed blockstate was unavailable; trying the next source"
                ),
                Err(message) => tracing::debug!(
                    source_id = %pack.id,
                    path = %state_path,
                    %message,
                    "blockstate source read failed; trying the next source"
                ),
            }
        }
        if let Some((bytes, blockstate_archive_path)) = blockstate_bytes {
            let blockstate: RawBlockstate = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(error) => {
                    tracing::debug!(path = %state_path, %error, "blockstate parse failed");
                    return self.no_preview(translation_key, "invalid blockstate", None);
                }
            };
            let property_values = blockstate.properties();
            let variants = blockstate.variants();
            let requested_variant = choose_variant(&variants, options.selected_variant.as_deref());
            let selected_state =
                select_blockstate(&property_values, requested_variant, &options.blockstate);
            // property selectorが送られたときは、古いvariant IDより現在のstateに合うvariantを優先する。
            let chosen = if has_requested_blockstate(&property_values, &options.blockstate) {
                choose_variant_for_state(&variants, &selected_state).or(requested_variant)
            } else {
                requested_variant
            };
            let selected_variant_id = chosen.map(|variant| variant.id.clone());
            let mut references: Vec<ModelReference> = chosen
                .map(|variant| vec![variant.selected_model.clone()])
                .unwrap_or_default();
            references.extend(
                variants
                    .iter()
                    .filter(|variant| variant.id.starts_with("multipart-"))
                    .filter(|variant| {
                        multipart_matches(variant.multipart_condition.as_ref(), &selected_state)
                    })
                    .map(|variant| variant.selected_model.clone()),
            );
            let properties = property_values
                .iter()
                .map(|(name, values)| BlockstateProperty {
                    name: name.clone(),
                    values: values.iter().cloned().collect(),
                    selected_value: selected_state[name].clone(),
                })
                .collect();
            let mut models = Vec::new();
            for reference in references {
                match self.resolve_model(&reference.model) {
                    Ok(model) => models.push(ResolvedBlockModel {
                        model,
                        x_rotation: reference.x,
                        y_rotation: reference.y,
                        uvlock: reference.uvlock,
                    }),
                    Err(error) => {
                        tracing::debug!(model = %reference.model, %error, "block model resolution failed");
                        let raw = self.raw_model_info(&reference.model).or_else(|| {
                            serde_json::from_slice::<Value>(&bytes)
                                .ok()
                                .map(|value| RawModelInfo {
                                    value,
                                    archive_path: blockstate_archive_path.clone(),
                                })
                        });
                        return self.no_preview(translation_key, &error.to_string(), raw);
                    }
                }
            }
            if models.is_empty() {
                return self.no_preview(translation_key, "blockstate has no model", None);
            }
            return PreviewManifest::Block(BlockPreviewManifest {
                translation_key: translation_key.to_owned(),
                variants,
                selected_variant: selected_variant_id,
                properties,
                selected_state,
                models,
            });
        }

        let direct_model = format!("{}:block/{}", resource.namespace, resource.id);
        match self.resolve_model(&direct_model) {
            Ok(model) => PreviewManifest::Block(BlockPreviewManifest {
                translation_key: translation_key.to_owned(),
                variants: Vec::new(),
                selected_variant: None,
                properties: Vec::new(),
                selected_state: BTreeMap::new(),
                models: vec![ResolvedBlockModel {
                    model,
                    x_rotation: 0,
                    y_rotation: 0,
                    uvlock: false,
                }],
            }),
            Err(error) => self.no_preview(
                translation_key,
                &error.to_string(),
                self.raw_model_info(&direct_model),
            ),
        }
    }

    fn resolve_item_preview(
        &self,
        translation_key: &str,
        resource: &TranslationResource,
        options: &PreviewOptions,
    ) -> PreviewManifest {
        let definition_path = format!("assets/{}/items/{}.json", resource.namespace, resource.id);
        // 新definitionは全sourceのlegacy modelより先に調べ、1.21.4形式が存在する時は必ず優先する。
        match self.read_exact_asset(&definition_path) {
            Ok(Some((source, bytes))) => {
                let definition: Value = match serde_json::from_slice(&bytes) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::debug!(path = %definition_path, %error, "item definition parse failed");
                        return self.no_preview(translation_key, "invalid item definition", None);
                    }
                };
                let mut state = ItemDefinitionBuild::default();
                let root_node = if definition.get("type").and_then(Value::as_str).is_some() {
                    &definition
                } else {
                    definition.get("model").unwrap_or(&definition)
                };
                match self.resolve_item_definition(root_node, "root", &options.item, &mut state, 0)
                {
                    Ok(resolved) => {
                        let model = state
                            .models
                            .first()
                            .cloned()
                            .unwrap_or_else(empty_item_model);
                        let layers = if model.generated_item {
                            generated_layers(&model.textures)
                        } else {
                            Vec::new()
                        };
                        PreviewManifest::Item(crate::ir::ItemPreviewManifest {
                            translation_key: translation_key.to_owned(),
                            model,
                            models: state.models,
                            layers,
                            definition: Some(resolved),
                            variants: state.variants,
                        })
                    }
                    Err(message) => {
                        tracing::debug!(path = %definition_path, source_id = %source.source_id, %message, "item definition resolution fell back");
                        self.no_preview(
                            translation_key,
                            &message,
                            Some(RawModelInfo {
                                value: definition,
                                archive_path: source.archive_path,
                            }),
                        )
                    }
                }
            }
            Ok(None) => self.resolve_legacy_item_preview(translation_key, resource),
            Err(error) => {
                tracing::debug!(path = %definition_path, %error, "item definition could not be read");
                self.no_preview(translation_key, &error.to_string(), None)
            }
        }
    }

    fn resolve_legacy_item_preview(
        &self,
        translation_key: &str,
        resource: &TranslationResource,
    ) -> PreviewManifest {
        let model_location = format!("{}:item/{}", resource.namespace, resource.id);
        match self.resolve_model(&model_location) {
            Ok(model) => {
                let layers = if model.generated_item {
                    generated_layers(&model.textures)
                } else {
                    Vec::new()
                };
                PreviewManifest::Item(crate::ir::ItemPreviewManifest {
                    translation_key: translation_key.to_owned(),
                    models: vec![model.clone()],
                    model,
                    layers,
                    definition: None,
                    variants: Vec::new(),
                })
            }
            Err(error) => self.no_preview(
                translation_key,
                &error.to_string(),
                self.raw_model_info(&model_location),
            ),
        }
    }

    fn resolve_item_definition(
        &self,
        node: &Value,
        selector: &str,
        selections: &BTreeMap<String, String>,
        state: &mut ItemDefinitionBuild,
        depth: usize,
    ) -> Result<ResolvedItemDefinition, String> {
        if depth >= MAX_MODEL_DEPTH {
            return Err("item definition exceeds the maximum nesting depth".to_owned());
        }
        let kind = node
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .strip_prefix("minecraft:")
            .unwrap_or_else(|| node.get("type").and_then(Value::as_str).unwrap_or_default());
        match kind {
            "model" => {
                let location = node
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "item model node has no model location".to_owned())?;
                let model = self
                    .resolve_model(location)
                    .map_err(|error| error.to_string())?;
                let model_index = state.models.len();
                state.models.push(model);
                // TintはMinecraftのcolor provider実行環境がないため、描画しなくてもIRから失わない。
                let tints = node
                    .get("tints")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                Ok(ResolvedItemDefinition::Model { model_index, tints })
            }
            "composite" => {
                let children = node
                    .get("children")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "composite item node has no children".to_owned())?;
                let children = children
                    .iter()
                    .enumerate()
                    .map(|(index, child)| {
                        self.resolve_item_definition(
                            child,
                            &format!("{selector}/children/{index}"),
                            selections,
                            state,
                            depth + 1,
                        )
                    })
                    .collect::<Result<_, _>>()?;
                Ok(ResolvedItemDefinition::Composite { children })
            }
            "condition" => {
                let property = item_property(node)?;
                // use-item等のruntime値はResolverから取得できないため、既定はfalse側に固定する。
                let selected = selections
                    .get(selector)
                    .filter(|choice| matches!(choice.as_str(), "false" | "true"))
                    .cloned()
                    .unwrap_or_else(|| "false".to_owned());
                state.variants.push(ItemDefinitionVariant {
                    selector: selector.to_owned(),
                    property: property.clone(),
                    selected: selected.clone(),
                    options: vec![
                        ItemDefinitionOption {
                            id: "false".to_owned(),
                            value: Some(Value::Bool(false)),
                            fallback: false,
                        },
                        ItemDefinitionOption {
                            id: "true".to_owned(),
                            value: Some(Value::Bool(true)),
                            fallback: false,
                        },
                    ],
                });
                let field = if selected == "true" {
                    "on_true"
                } else {
                    "on_false"
                };
                let child = if let Some(child) = node.get(field) {
                    self.resolve_item_definition(
                        child,
                        &format!("{selector}/{selected}"),
                        selections,
                        state,
                        depth + 1,
                    )?
                } else {
                    ResolvedItemDefinition::Empty
                };
                Ok(ResolvedItemDefinition::Condition {
                    property,
                    selector: selector.to_owned(),
                    selected,
                    child: Box::new(child),
                })
            }
            "select" => {
                let property = item_property(node)?;
                let cases = node
                    .get("cases")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let fallback = node.get("fallback");
                let mut options: Vec<_> = cases
                    .iter()
                    .enumerate()
                    .map(|(index, case)| ItemDefinitionOption {
                        id: format!("case-{index}"),
                        value: case.get("when").cloned(),
                        fallback: false,
                    })
                    .collect();
                if fallback.is_some() {
                    options.push(ItemDefinitionOption {
                        id: "fallback".to_owned(),
                        value: None,
                        fallback: true,
                    });
                }
                // property値がないPreviewでは、定義されたfallbackを優先し、なければ先頭caseを使う。
                let default = if fallback.is_some() {
                    "fallback".to_owned()
                } else if cases.is_empty() {
                    return Err("select item node has no cases or fallback".to_owned());
                } else {
                    "case-0".to_owned()
                };
                let selected = selections
                    .get(selector)
                    .filter(|choice| options.iter().any(|option| &option.id == *choice))
                    .cloned()
                    .unwrap_or(default);
                state.variants.push(ItemDefinitionVariant {
                    selector: selector.to_owned(),
                    property: property.clone(),
                    selected: selected.clone(),
                    options,
                });
                let child = if selected == "fallback" {
                    fallback
                } else {
                    selected
                        .strip_prefix("case-")
                        .and_then(|index| index.parse::<usize>().ok())
                        .and_then(|index| cases.get(index))
                        .and_then(|case| case.get("model"))
                };
                let child = if let Some(child) = child {
                    self.resolve_item_definition(
                        child,
                        &format!("{selector}/{selected}"),
                        selections,
                        state,
                        depth + 1,
                    )?
                } else {
                    ResolvedItemDefinition::Empty
                };
                Ok(ResolvedItemDefinition::Select {
                    property,
                    selector: selector.to_owned(),
                    selected,
                    child: Box::new(child),
                })
            }
            "range_dispatch" => {
                let property = item_property(node)?;
                let entries = node
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let mut ordered: Vec<_> = entries
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| {
                        entry
                            .get("threshold")
                            .and_then(Value::as_f64)
                            .map(|threshold| (index, threshold, entry))
                    })
                    .collect();
                ordered.sort_by(|left, right| {
                    left.1
                        .total_cmp(&right.1)
                        .then_with(|| left.0.cmp(&right.0))
                });
                let fallback = node.get("fallback");
                let mut options: Vec<_> = ordered
                    .iter()
                    .map(|(index, threshold, _)| ItemDefinitionOption {
                        id: format!("entry-{index}"),
                        value: serde_json::Number::from_f64(*threshold).map(Value::Number),
                        fallback: false,
                    })
                    .collect();
                if fallback.is_some() {
                    options.push(ItemDefinitionOption {
                        id: "fallback".to_owned(),
                        value: None,
                        fallback: true,
                    });
                }
                // range propertyの実値を知らない場合、最小threshold側が決定的な初期表示になる。
                let default = if let Some((index, _, _)) = ordered.first() {
                    format!("entry-{index}")
                } else if fallback.is_some() {
                    "fallback".to_owned()
                } else {
                    return Err("range_dispatch item node has no entries or fallback".to_owned());
                };
                let selected = selections
                    .get(selector)
                    .filter(|choice| options.iter().any(|option| &option.id == *choice))
                    .cloned()
                    .unwrap_or(default);
                state.variants.push(ItemDefinitionVariant {
                    selector: selector.to_owned(),
                    property: property.clone(),
                    selected: selected.clone(),
                    options,
                });
                let child = if selected == "fallback" {
                    fallback
                } else {
                    selected
                        .strip_prefix("entry-")
                        .and_then(|index| index.parse::<usize>().ok())
                        .and_then(|index| entries.get(index))
                        .and_then(|entry| entry.get("model"))
                };
                let child = if let Some(child) = child {
                    self.resolve_item_definition(
                        child,
                        &format!("{selector}/{selected}"),
                        selections,
                        state,
                        depth + 1,
                    )?
                } else {
                    ResolvedItemDefinition::Empty
                };
                Ok(ResolvedItemDefinition::RangeDispatch {
                    property,
                    selector: selector.to_owned(),
                    selected,
                    child: Box::new(child),
                })
            }
            "empty" => Ok(ResolvedItemDefinition::Empty),
            "special" => {
                let base = node
                    .get("base")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "special item definition has no base model".to_owned())?;
                let renderer = node
                    .get("model")
                    .and_then(|model| model.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown special renderer");
                let mut model = self
                    .resolve_model(base)
                    .map_err(|error| error.to_string())?;
                if model.elements.is_empty() {
                    return Err(format!(
                        "special item renderer {renderer} has no static base geometry"
                    ));
                }
                // Bannerやchest等はruntime rendererの模様・状態を再現できないため、定義のbase形状を代わりに表示する。
                model.unsupported_features.push(format!(
                    "special item renderer {renderer} is unavailable; displaying its base model"
                ));
                let model_index = state.models.len();
                state.models.push(model);
                Ok(ResolvedItemDefinition::Model {
                    model_index,
                    tints: Vec::new(),
                })
            }
            "bundle/selected_item" => Err(format!(
                "item definition node {kind} requires an unsupported renderer"
            )),
            _ => Err(format!("unsupported item definition node: {kind}")),
        }
    }

    fn resolve_entity_preview(
        &self,
        translation_key: &str,
        resource: &TranslationResource,
        options: &PreviewOptions,
    ) -> PreviewManifest {
        let mut geo_assets: Vec<_> = self
            .index
            .in_namespace(&resource.namespace)
            .filter(|asset| asset.asset_type == AssetType::GeoModel)
            .filter_map(|asset| {
                candidate_confidence(asset, resource).map(|confidence| (asset, confidence))
            })
            .collect();
        geo_assets.sort_by(|(left, left_confidence), (right, right_confidence)| {
            confidence_rank(*left_confidence)
                .cmp(&confidence_rank(*right_confidence))
                .then_with(|| left.archive_path.cmp(&right.archive_path))
                .then_with(|| {
                    self.source_rank(&left.source_id)
                        .cmp(&self.source_rank(&right.source_id))
                })
        });
        geo_assets.dedup_by(|left, right| left.0.archive_path == right.0.archive_path);
        for (asset, confidence) in geo_assets {
            let Ok(Some((source, bytes))) = self.read_exact_asset(&asset.archive_path) else {
                continue;
            };
            let Some((geometry, identifier_matches)) = parse_geo_document(&bytes, &resource.id)
            else {
                tracing::debug!(path = %asset.archive_path, "GeckoLib geometry was unsupported or invalid");
                continue;
            };
            // ファイル名がEntity keyと一致してもgeometry identifierが違う場合、確定候補に見せない。
            let confidence = if identifier_matches {
                confidence
            } else {
                LinkConfidence::Low
            };
            let texture_candidates =
                self.entity_texture_candidates(resource, Some(&geometry.identifier));
            let selected_texture = options
                .selected_texture
                .as_deref()
                .and_then(|selected| {
                    texture_candidates.iter().find(|candidate| {
                        format!(
                            "{}:{}",
                            candidate.asset.source_id, candidate.asset.archive_path
                        ) == selected
                    })
                })
                .or_else(|| texture_candidates.first());
            let (texture, texture_confidence) = selected_texture
                .map(|candidate| {
                    let location = format!(
                        "{}:{}",
                        candidate.asset.namespace, candidate.asset.resource_id
                    );
                    (
                        Some(TextureReference {
                            location,
                            path: Some(candidate.asset.clone()),
                            missing: false,
                            searched_sources: vec![candidate.asset.source_id.clone()],
                        }),
                        Some(candidate.confidence),
                    )
                })
                .unwrap_or((None, None));
            return PreviewManifest::Entity(EntityPreviewManifest {
                translation_key: translation_key.to_owned(),
                geometry,
                geometry_source: source,
                confidence,
                texture,
                texture_confidence,
                texture_candidates,
            });
        }

        let candidates = self.entity_texture_candidates(resource, None);
        if candidates.is_empty() {
            return PreviewManifest::NoPreview(NoPreviewManifest {
                translation_key: translation_key.to_owned(),
                reason: "entity geometry and texture are unavailable".to_owned(),
                texture_candidates: candidates,
                raw_model_info: None,
                raw_model_path: None,
            });
        }
        let textures = candidates
            .iter()
            .map(|candidate| TextureReference {
                location: format!(
                    "{}:{}",
                    candidate.asset.namespace, candidate.asset.resource_id
                ),
                path: Some(candidate.asset.clone()),
                missing: false,
                searched_sources: vec![candidate.asset.source_id.clone()],
            })
            .collect();
        PreviewManifest::Texture(TexturePreviewManifest {
            translation_key: translation_key.to_owned(),
            textures,
            candidates,
            reason: None,
            raw_model_info: None,
        })
    }

    fn entity_texture_candidates(
        &self,
        resource: &TranslationResource,
        geometry_identifier: Option<&str>,
    ) -> Vec<TextureCandidate> {
        let geometry_name = geometry_identifier
            .map(|identifier| {
                identifier
                    .strip_prefix("geometry.")
                    .unwrap_or(identifier)
                    .rsplit(['/', '.'])
                    .next()
                    .unwrap_or(identifier)
            })
            .unwrap_or_default();
        let mut candidates: Vec<_> = self
            .index
            .in_namespace(&resource.namespace)
            .filter(|asset| asset.asset_type == AssetType::TextureEntity)
            .filter_map(|asset| {
                let direct = candidate_confidence(asset, resource);
                let candidate_name = asset.resource_id.rsplit('/').next()?;
                let confidence = if direct == Some(LinkConfidence::High) {
                    LinkConfidence::High
                } else if !geometry_name.is_empty() && candidate_name == geometry_name {
                    LinkConfidence::Medium
                } else {
                    direct?
                };
                Some(TextureCandidate {
                    asset: AssetLocation::from(asset),
                    confidence,
                    confirmed: confidence != LinkConfidence::Low,
                })
            })
            .collect();
        candidates.sort_by(|left, right| {
            confidence_rank(left.confidence)
                .cmp(&confidence_rank(right.confidence))
                .then_with(|| {
                    self.source_rank(&left.asset.source_id)
                        .cmp(&self.source_rank(&right.asset.source_id))
                })
                .then_with(|| left.asset.archive_path.cmp(&right.asset.archive_path))
        });
        candidates
    }

    fn source_rank(&self, source_id: &str) -> usize {
        self.packs
            .iter()
            .position(|pack| pack.id == source_id)
            .unwrap_or(usize::MAX)
    }

    fn no_preview(
        &self,
        translation_key: &str,
        reason: &str,
        raw_model_info: Option<RawModelInfo>,
    ) -> PreviewManifest {
        let raw_model_path = raw_model_info
            .as_ref()
            .map(|raw_model| raw_model.archive_path.clone());
        let raw_model_info = raw_model_info.map(|raw_model| raw_model.value);
        let mut candidates = parse_translation_key(translation_key)
            .map(|resource| self.texture_candidates(&resource))
            .unwrap_or_default();
        let referenced_textures = raw_model_info
            .as_ref()
            .map(|raw_model| self.raw_model_texture_references(raw_model))
            .unwrap_or_default();
        for texture in &referenced_textures {
            let Some(asset) = texture.path.as_ref() else {
                continue;
            };
            if !candidates.iter().any(|candidate| candidate.asset == *asset) {
                candidates.push(TextureCandidate {
                    asset: asset.clone(),
                    confidence: LinkConfidence::High,
                    confirmed: true,
                });
            }
        }
        if !candidates.is_empty() || !referenced_textures.is_empty() {
            let mut textures: Vec<_> = candidates
                .iter()
                .map(|candidate| TextureReference {
                    location: format!(
                        "{}:{}",
                        candidate.asset.namespace, candidate.asset.resource_id
                    ),
                    path: Some(candidate.asset.clone()),
                    missing: false,
                    searched_sources: vec![candidate.asset.source_id.clone()],
                })
                .collect();
            for texture in referenced_textures {
                if !textures.iter().any(|existing| {
                    existing.location == texture.location && existing.path == texture.path
                }) {
                    textures.push(texture);
                }
            }
            return PreviewManifest::Texture(TexturePreviewManifest {
                translation_key: translation_key.to_owned(),
                textures,
                candidates,
                reason: Some(reason.to_owned()),
                raw_model_info,
            });
        }
        PreviewManifest::NoPreview(NoPreviewManifest {
            translation_key: translation_key.to_owned(),
            reason: reason.to_owned(),
            texture_candidates: candidates,
            raw_model_info,
            raw_model_path,
        })
    }

    fn texture_candidates(&self, resource: &TranslationResource) -> Vec<TextureCandidate> {
        self.links_for_translation_key(&format!(
            "{}.{}.{}",
            match resource.kind {
                TranslationKind::Block => "block",
                TranslationKind::Item => "item",
                TranslationKind::Entity => "entity",
            },
            resource.namespace,
            resource.id
        ))
        .into_iter()
        .filter(|link| {
            matches!(
                link.asset.asset_type,
                AssetType::TextureBlock | AssetType::TextureItem | AssetType::TextureEntity
            )
        })
        .map(|link| TextureCandidate {
            asset: link.asset,
            confidence: link.confidence,
            confirmed: link.confirmed,
        })
        .collect()
    }

    fn raw_model_texture_references(&self, raw_model: &Value) -> Vec<TextureReference> {
        self.raw_model_texture_references_inner(raw_model, &mut HashSet::new(), 0)
    }

    fn raw_model_texture_references_inner(
        &self,
        raw_model: &Value,
        visited_models: &mut HashSet<String>,
        depth: usize,
    ) -> Vec<TextureReference> {
        if depth >= MAX_MODEL_DEPTH {
            return Vec::new();
        }
        let mut texture_variables = BTreeMap::new();
        let mut resolved_texture_variables = HashMap::new();
        let mut values = BTreeSet::new();
        if let Some(textures) = raw_model.get("textures").and_then(Value::as_object) {
            for (name, value) in textures {
                if let Some(value) = value.as_str() {
                    texture_variables.insert(name.clone(), value.to_owned());
                    values.insert(format!("#{name}"));
                }
            }
        }
        if let Some(elements) = raw_model.get("elements").and_then(Value::as_array) {
            for faces in elements
                .iter()
                .filter_map(|element| element.get("faces").and_then(Value::as_object))
            {
                for value in faces
                    .values()
                    .filter_map(|face| face.get("texture").and_then(Value::as_str))
                {
                    values.insert(value.to_owned());
                }
            }
        }

        let mut seen_locations = BTreeSet::new();
        let mut references: Vec<_> = values
            .into_iter()
            .filter_map(|value| {
                let location = if let Some(variable) = value.strip_prefix('#') {
                    self.resolve_texture_variable(
                        variable,
                        &texture_variables,
                        &mut resolved_texture_variables,
                    )
                    .ok()?
                } else {
                    parse_resource_location(&value).ok()?.to_string()
                };
                if !seen_locations.insert(location.clone()) {
                    return None;
                }
                self.texture_reference(&location).ok()
            })
            .collect();

        let mut model_locations = Vec::new();
        collect_item_model_locations(raw_model, &mut model_locations);
        for model_location in model_locations {
            if !visited_models.insert(model_location.clone()) {
                continue;
            }
            if let Ok(model) = self.resolve_model(&model_location) {
                references.extend(model.textures.into_values());
            } else if let Some(raw_model) = self.raw_model_info(&model_location) {
                references.extend(self.raw_model_texture_references_inner(
                    &raw_model.value,
                    visited_models,
                    depth + 1,
                ));
            }
            visited_models.remove(&model_location);
        }
        let mut seen_references = BTreeSet::new();
        references
            .into_iter()
            .filter(|reference| {
                seen_references.insert((
                    reference.location.clone(),
                    reference
                        .path
                        .as_ref()
                        .map(|path| (path.source_id.clone(), path.archive_path.clone())),
                ))
            })
            .collect()
    }

    fn resolve_model_chain(
        &self,
        location: &ResourceLocation,
        stack: &mut Vec<String>,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> Result<(ModelChain, AssetLocation), ResolverError> {
        if depth >= MAX_MODEL_DEPTH {
            return Err(ResolverError::MaximumModelDepth);
        }
        let identity = location.to_string();
        if !visited.insert(identity.clone()) {
            stack.push(identity);
            return Err(ResolverError::ParentCycle(stack.clone()));
        }
        if location.namespace == "minecraft"
            && location.path.starts_with("builtin/")
            && location.path != "builtin/generated"
        {
            return Err(ResolverError::UnsupportedModel(identity));
        }
        stack.push(identity.clone());
        let (asset, bytes) = self.read_model_candidate(location)?;
        let raw_model: RawModel =
            serde_json::from_slice(&bytes).map_err(|error| ResolverError::InvalidModelJson {
                source_id: asset.source_id.clone(),
                path: asset.archive_path.clone(),
                message: error.to_string(),
            })?;
        // Banner/bed/chest等はvanilla JSONがparticle textureだけを持ち、実形状をBlockEntityRendererへ任せる。
        // Standalone previewではruntime stateを再現できないため、既知のmodelだけ代表形へ置き換える。
        let fallback_elements = (raw_model.parent.is_none() && raw_model.elements.is_none())
            .then(|| builtin::fallback_elements(&location.path))
            .flatten();
        let has_explicit_elements = raw_model.elements.is_some();

        let mut chain = if let Some(parent) = raw_model.parent.as_deref() {
            let parent_location = parse_resource_location(parent)?;
            self.resolve_model_chain(&parent_location, stack, visited, depth + 1)?
                .0
        } else {
            ModelChain {
                source_chain: Vec::new(),
                textures: BTreeMap::new(),
                elements: fallback_elements.unwrap_or_default(),
                ambient_occlusion: true,
                display: BTreeMap::new(),
                gui_light: None,
                generated_item: false,
                unsupported_features: Vec::new(),
            }
        };
        for (name, texture) in raw_model.textures {
            chain.textures.insert(name, texture);
        }
        if let Some(elements) = raw_model.elements {
            chain.elements = elements;
        }
        if let Some(ambient_occlusion) = raw_model.ambient_occlusion {
            chain.ambient_occlusion = ambient_occlusion;
        }
        chain.display.extend(raw_model.display);
        if raw_model.gui_light.is_some() {
            chain.gui_light = raw_model.gui_light;
        }
        chain.generated_item |=
            location.path == "item/generated" || location.path == "builtin/generated";
        let has_custom_loader = raw_model.loader.is_some();
        if let Some(loader) = raw_model.loader {
            // loader固有geometryを読み込めない場合でもparticle面を立方体に貼り、未対応方式をIRに残す。
            chain
                .unsupported_features
                .push(format!("unsupported model loader: {loader}"));
        }
        let is_intentionally_invisible = location.namespace == "minecraft"
            && (matches!(
                location.path.as_str(),
                "block/air" | "block/barrier" | "block/structure_void"
            ) || location.path.starts_with("block/light_"));
        if chain.elements.is_empty()
            && chain.textures.contains_key("particle")
            && (!has_explicit_elements || has_custom_loader)
            && !is_intentionally_invisible
        {
            chain.elements.push(builtin::particle_cube());
            if !has_custom_loader {
                // JSONにparticle textureだけある静的Mod modelには描画形状がないため、単独Preview用cubeとして示す。
                chain.unsupported_features.push(
                    "model has no elements; displaying its particle texture on a cube".to_owned(),
                );
            }
        }
        // childからparentの順で残し、Model modeとbinary許可リストの両方から継承元を追えるようにする。
        chain.source_chain.insert(0, asset.clone());
        stack.pop();
        visited.remove(&identity);
        Ok((chain, asset))
    }

    fn resolve_texture_value(
        &self,
        value: &str,
        textures: &BTreeMap<String, String>,
        resolved: &mut HashMap<String, String>,
    ) -> Result<String, ResolverError> {
        if let Some(variable) = value.strip_prefix('#') {
            return self.resolve_texture_variable(variable, textures, resolved);
        }
        Ok(parse_resource_location(value)?.to_string())
    }

    // Texture aliases may be chained by mod models. Resolve and memoize them iteratively so input
    // size cannot consume the Rust call stack; a repeated alias still makes just this model invalid.
    fn resolve_texture_variable<'a>(
        &self,
        variable: &'a str,
        textures: &'a BTreeMap<String, String>,
        resolved: &mut HashMap<String, String>,
    ) -> Result<String, ResolverError> {
        let mut pending: Vec<&'a str> = Vec::new();
        let mut visited = HashSet::new();
        let mut current = variable;
        loop {
            if let Some(location) = resolved.get(current) {
                let location = location.clone();
                for name in pending {
                    resolved.insert(name.to_owned(), location.clone());
                }
                return Ok(location);
            }
            if !visited.insert(current) {
                let mut cycle: Vec<_> = pending.iter().map(|name| (*name).to_owned()).collect();
                cycle.push(current.to_owned());
                return Err(ResolverError::TextureVariableCycle(cycle));
            }
            let value = textures
                .get(current)
                .ok_or_else(|| ResolverError::MissingTextureVariable(current.to_owned()))?;
            pending.push(current);
            if let Some(parent) = value.strip_prefix('#') {
                current = parent;
                continue;
            }

            let location = parse_resource_location(value)?.to_string();
            for name in pending {
                resolved.insert(name.to_owned(), location.clone());
            }
            return Ok(location);
        }
    }

    fn texture_reference(&self, location: &str) -> Result<TextureReference, ResolverError> {
        let parsed = parse_resource_location(location)?;
        let path = format!("assets/{}/textures/{}.png", parsed.namespace, parsed.path);
        let mut searched_sources = Vec::new();
        for pack in &self.packs {
            searched_sources.push(pack.id.clone());
            if let Some(asset) = self.index.find(&pack.id, &path) {
                return Ok(TextureReference {
                    location: parsed.to_string(),
                    path: Some(AssetLocation::from(asset)),
                    missing: false,
                    searched_sources,
                });
            }
            if let Some(asset) = self.index.find_custom_texture(&pack.id, &path) {
                // AssetLocation has no generic custom-texture kind in its stable wire schema.
                // Rendering keys by source and archive path, so preserve that contract here.
                return Ok(TextureReference {
                    location: parsed.to_string(),
                    path: Some(AssetLocation {
                        source_id: asset.source_id.clone(),
                        source_kind: asset.source_kind,
                        archive_path: asset.archive_path.clone(),
                        namespace: asset.namespace.clone(),
                        resource_id: asset.resource_id.clone(),
                        asset_type: AssetType::TextureBlock,
                    }),
                    missing: false,
                    searched_sources,
                });
            }
        }
        Ok(TextureReference {
            location: parsed.to_string(),
            path: None,
            missing: true,
            searched_sources,
        })
    }

    fn read_exact_asset(
        &self,
        path: &str,
    ) -> Result<Option<(AssetLocation, Vec<u8>)>, ResolverError> {
        let mut last_read_error = None;
        for pack in &self.packs {
            let Some(asset) = self.index.find(&pack.id, path) else {
                continue;
            };
            match pack.source.read(&asset.archive_path) {
                Ok(Some(bytes)) => return Ok(Some((AssetLocation::from(asset), bytes))),
                Ok(None) => tracing::debug!(
                    source_id = %pack.id,
                    path = %asset.archive_path,
                    "indexed asset was unavailable; trying the next source"
                ),
                Err(message) => {
                    tracing::debug!(
                        source_id = %pack.id,
                        path = %asset.archive_path,
                        %message,
                        "asset source read failed; trying the next source"
                    );
                    last_read_error = Some(SourceReadError {
                        source_id: pack.id.clone(),
                        path: asset.archive_path.clone(),
                        message,
                    });
                }
            }
        }
        if let Some(error) = last_read_error {
            return Err(error.into());
        }
        Ok(None)
    }

    fn raw_model_info(&self, model: &str) -> Option<RawModelInfo> {
        let location = parse_resource_location(model).ok()?;
        let (source, bytes) = self.read_model_candidate(&location).ok()?;
        Some(RawModelInfo {
            value: serde_json::from_slice(&bytes).ok()?,
            archive_path: source.archive_path,
        })
    }

    fn read_model_candidate(
        &self,
        location: &ResourceLocation,
    ) -> Result<(AssetLocation, Vec<u8>), ResolverError> {
        let path = format!(
            "assets/{}/models/{}.json",
            location.namespace, location.path
        );
        let mut last_read_error = None;
        for pack in &self.packs {
            let Some(asset) = self.index.find(&pack.id, &path) else {
                continue;
            };
            match pack.source.read(&asset.archive_path) {
                Ok(Some(bytes)) => return Ok((AssetLocation::from(asset), bytes)),
                Ok(None) => tracing::debug!(
                    source_id = %pack.id,
                    path = %asset.archive_path,
                    "indexed model was unavailable; trying the next source"
                ),
                Err(message) => {
                    tracing::debug!(
                        source_id = %pack.id,
                        path = %asset.archive_path,
                        %message,
                        "model source read failed; trying the next source"
                    );
                    last_read_error = Some(SourceReadError {
                        source_id: pack.id.clone(),
                        path: asset.archive_path.clone(),
                        message,
                    });
                }
            }
        }
        if let Some(error) = last_read_error {
            return Err(error.into());
        }
        Err(ResolverError::ModelNotFound(location.to_string()))
    }
}

struct BuiltinSource(BTreeMap<String, Vec<u8>>);

impl AssetSource for BuiltinSource {
    fn paths(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.get(path).cloned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResourceLocation {
    namespace: String,
    path: String,
}

impl std::fmt::Display for ResourceLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.namespace, self.path)
    }
}

fn parse_resource_location(value: &str) -> Result<ResourceLocation, ResolverError> {
    let (namespace, path) = value.split_once(':').unwrap_or(("minecraft", value));
    if namespace.is_empty()
        || path.is_empty()
        || namespace.contains('/')
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return Err(ResolverError::InvalidResourceLocation(value.to_owned()));
    }
    Ok(ResourceLocation {
        namespace: namespace.to_owned(),
        path: path.to_owned(),
    })
}

fn parse_translation_key(key: &str) -> Option<TranslationResource> {
    let (prefix, remaining) = key.split_once('.')?;
    let (namespace, id) = remaining.split_once('.')?;
    let kind = match prefix {
        "block" => TranslationKind::Block,
        "item" => TranslationKind::Item,
        "entity" => TranslationKind::Entity,
        _ => return None,
    };
    if namespace.is_empty() || id.is_empty() {
        return None;
    }
    Some(TranslationResource {
        kind,
        namespace: namespace.to_owned(),
        id: id.to_owned(),
    })
}

fn candidate_confidence(
    asset: &crate::index::AssetPath,
    resource: &TranslationResource,
) -> Option<LinkConfidence> {
    let expected_types: &[AssetType] = match resource.kind {
        TranslationKind::Block => &[
            AssetType::Blockstate,
            AssetType::BlockModel,
            AssetType::TextureBlock,
        ],
        TranslationKind::Item => &[
            AssetType::ItemModel,
            AssetType::ItemDefinition,
            AssetType::TextureItem,
        ],
        TranslationKind::Entity => &[AssetType::TextureEntity, AssetType::GeoModel],
    };
    if !expected_types.contains(&asset.asset_type) {
        return None;
    }
    let expected_path = match asset.asset_type {
        AssetType::Blockstate => format!(
            "assets/{}/blockstates/{}.json",
            resource.namespace, resource.id
        ),
        AssetType::BlockModel => format!(
            "assets/{}/models/block/{}.json",
            resource.namespace, resource.id
        ),
        AssetType::ItemModel => format!(
            "assets/{}/models/item/{}.json",
            resource.namespace, resource.id
        ),
        AssetType::ItemDefinition => {
            format!("assets/{}/items/{}.json", resource.namespace, resource.id)
        }
        AssetType::TextureBlock => format!(
            "assets/{}/textures/block/{}.png",
            resource.namespace, resource.id
        ),
        AssetType::TextureItem => format!(
            "assets/{}/textures/item/{}.png",
            resource.namespace, resource.id
        ),
        AssetType::TextureEntity => format!(
            "assets/{}/textures/entity/{}.png",
            resource.namespace, resource.id
        ),
        AssetType::GeoModel => {
            format!("assets/{}/geo/{}.geo.json", resource.namespace, resource.id)
        }
        AssetType::Animation | AssetType::OtherModel => return None,
    };
    if asset.archive_path == expected_path {
        return Some(LinkConfidence::High);
    }
    let wanted_name = resource.id.rsplit('/').next()?;
    let candidate_name = asset.resource_id.rsplit('/').next()?;
    if candidate_name == wanted_name {
        return Some(LinkConfidence::Medium);
    }
    let wanted_tokens: BTreeSet<_> = wanted_name
        .split(['_', '-'])
        .filter(|part| part.len() > 1)
        .collect();
    let candidate_tokens: BTreeSet<_> = candidate_name
        .split(['_', '-'])
        .filter(|part| part.len() > 1)
        .collect();
    (wanted_tokens.len() > 1 && !wanted_tokens.is_disjoint(&candidate_tokens))
        .then_some(LinkConfidence::Low)
}

fn confidence_rank(confidence: LinkConfidence) -> u8 {
    match confidence {
        LinkConfidence::High => 0,
        LinkConfidence::Medium => 1,
        LinkConfidence::Low => 2,
    }
}

#[derive(Default)]
struct ItemDefinitionBuild {
    models: Vec<ResolvedModel>,
    variants: Vec<ItemDefinitionVariant>,
}

fn item_property(node: &Value) -> Result<String, String> {
    node.get("property")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "item definition branch has no property".to_owned())
}

fn collect_item_model_locations(value: &Value, locations: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            let kind = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .strip_prefix("minecraft:")
                .unwrap_or_else(|| {
                    object
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                });
            let field = match kind {
                "model" => Some("model"),
                "special" => Some("base"),
                _ => None,
            };
            if let Some(location) = field
                .and_then(|field| object.get(field))
                .and_then(Value::as_str)
            {
                locations.push(location.to_owned());
            }
            for child in object.values() {
                collect_item_model_locations(child, locations);
            }
        }
        Value::Array(children) => {
            for child in children {
                collect_item_model_locations(child, locations);
            }
        }
        _ => {}
    }
}

fn empty_item_model() -> ResolvedModel {
    ResolvedModel {
        model: "minecraft:empty".to_owned(),
        source: None,
        source_chain: Vec::new(),
        textures: BTreeMap::new(),
        elements: Vec::new(),
        ambient_occlusion: true,
        display: BTreeMap::new(),
        gui_light: None,
        generated_item: false,
        unsupported_features: Vec::new(),
    }
}

fn parse_geo_document(bytes: &[u8], entity_id: &str) -> Option<(EntityGeometry, bool)> {
    // 1.12.0以降のminecraft:geometryと、1.8.0のgeometry.<id>を静的geometryとして読む。
    // animation resourceはindexするが、再生状態が別途必要なためこのIRには含めない。
    let document: Value = serde_json::from_slice(bytes).ok()?;
    let format_version = document.get("format_version")?.as_str()?.to_owned();
    let version = parse_version(&format_version)?;
    let is_modern = version >= (1, 12, 0);
    let is_legacy = version == (1, 8, 0);
    if !is_modern && !is_legacy {
        return None;
    }

    let mut definitions = Vec::new();
    if let Some(geometries) = document.get("minecraft:geometry").and_then(Value::as_array) {
        definitions.extend(
            geometries
                .iter()
                .cloned()
                .map(|definition| (String::new(), definition)),
        );
    }
    if is_legacy && let Some(object) = document.as_object() {
        definitions.extend(
            object
                .iter()
                .filter(|(key, _)| key.starts_with("geometry."))
                .map(|(key, definition)| (key.clone(), definition.clone())),
        );
    }
    if definitions.is_empty() {
        return None;
    }

    let mut parsed = Vec::new();
    for (legacy_identifier, definition) in definitions {
        let raw: RawGeoGeometry = serde_json::from_value(definition).ok()?;
        let identifier = raw
            .description
            .as_ref()
            .and_then(|description| description.identifier.clone())
            .or_else(|| (!legacy_identifier.is_empty()).then_some(legacy_identifier))?;
        let geometry = EntityGeometry {
            identifier: identifier.clone(),
            format_version: format_version.clone(),
            texture_width: raw
                .description
                .as_ref()
                .and_then(|description| description.texture_width)
                .or(raw.texture_width),
            texture_height: raw
                .description
                .as_ref()
                .and_then(|description| description.texture_height)
                .or(raw.texture_height),
            bones: raw
                .bones
                .into_iter()
                .map(|bone| EntityBone {
                    name: bone.name,
                    parent: bone.parent,
                    pivot: bone.pivot,
                    rotation: bone.rotation,
                    cubes: bone
                        .cubes
                        .into_iter()
                        .map(|cube| EntityCube {
                            origin: cube.origin,
                            size: cube.size,
                            pivot: cube.pivot,
                            rotation: cube.rotation,
                            uv: cube.uv.map(Into::into),
                            inflate: cube.inflate,
                            mirror: cube.mirror,
                        })
                        .collect(),
                })
                .collect(),
        };
        let normalized_identifier = identifier.strip_prefix("geometry.").unwrap_or(&identifier);
        let normalized_entity = entity_id.strip_prefix("geometry.").unwrap_or(entity_id);
        let score = if normalized_identifier == normalized_entity {
            2
        } else if normalized_identifier.rsplit(['/', '.']).next()
            == normalized_entity.rsplit(['/', '.']).next()
        {
            1
        } else {
            0
        };
        parsed.push((score, geometry));
    }
    parsed
        .into_iter()
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(score, geometry)| (geometry, score != 0))
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.split(['-', '+']).next()?;
    let mut parts = version.split('.').map(str::parse::<u64>);
    Some((
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ))
}

// GeckoLib 1.12+ descriptionはsnake_case、1.8.0形式はtexturewidth表記なので両方読む。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawGeoGeometry {
    #[serde(default)]
    description: Option<RawGeoDescription>,
    #[serde(default, alias = "texturewidth")]
    texture_width: Option<u32>,
    #[serde(default, alias = "textureheight")]
    texture_height: Option<u32>,
    #[serde(default)]
    bones: Vec<RawGeoBone>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawGeoDescription {
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default, alias = "texturewidth")]
    texture_width: Option<u32>,
    #[serde(default, alias = "textureheight")]
    texture_height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawGeoBone {
    #[serde(default)]
    name: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    pivot: Option<[f64; 3]>,
    #[serde(default)]
    rotation: Option<[f64; 3]>,
    #[serde(default)]
    cubes: Vec<RawGeoCube>,
}

#[derive(Debug, Deserialize)]
struct RawGeoCube {
    origin: [f64; 3],
    size: [f64; 3],
    #[serde(default)]
    pivot: Option<[f64; 3]>,
    #[serde(default)]
    rotation: Option<[f64; 3]>,
    #[serde(default)]
    uv: Option<RawGeoCubeUv>,
    #[serde(default)]
    inflate: f64,
    #[serde(default)]
    mirror: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawGeoCubeUv {
    Box([f64; 2]),
    PerFace(BTreeMap<crate::ir::FaceName, RawGeoFaceUv>),
}

#[derive(Debug, Deserialize)]
struct RawGeoFaceUv {
    uv: [f64; 2],
    #[serde(default)]
    uv_size: [f64; 2],
}

impl From<RawGeoCubeUv> for EntityCubeUv {
    fn from(uv: RawGeoCubeUv) -> Self {
        match uv {
            RawGeoCubeUv::Box(uv) => Self::Box(uv),
            RawGeoCubeUv::PerFace(faces) => Self::PerFace(
                faces
                    .into_iter()
                    .map(|(face, uv)| {
                        (
                            face,
                            EntityFaceUv {
                                uv: uv.uv,
                                uv_size: uv.uv_size,
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawBlockstate {
    #[serde(default)]
    variants: BTreeMap<String, ModelList>,
    #[serde(default)]
    multipart: Vec<RawMultipart>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ModelList {
    One(ModelReference),
    Many(Vec<ModelReference>),
}

impl ModelList {
    fn into_vec(self) -> Vec<ModelReference> {
        match self {
            Self::One(model) => vec![model],
            Self::Many(models) => models,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawMultipart {
    #[serde(default)]
    when: Option<Value>,
    apply: ModelList,
}

impl<'de> Deserialize<'de> for ModelReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            model: String,
            #[serde(default)]
            x: i32,
            #[serde(default)]
            y: i32,
            #[serde(default)]
            uvlock: bool,
            #[serde(default = "default_weight")]
            weight: u32,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            model: raw.model,
            x: raw.x,
            y: raw.y,
            uvlock: raw.uvlock,
            weight: raw.weight,
        })
    }
}

fn default_weight() -> u32 {
    1
}

impl RawBlockstate {
    fn properties(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut properties = BTreeMap::new();
        for selector in self.variants.keys() {
            for (name, values) in parse_variant_selector(selector) {
                properties
                    .entry(name)
                    .or_insert_with(BTreeSet::new)
                    .extend(values);
            }
        }
        for multipart in &self.multipart {
            if let Some(when) = &multipart.when {
                collect_condition_properties(when, &mut properties);
            }
        }
        properties
    }

    fn variants(self) -> Vec<BlockstateVariant> {
        let mut variants: Vec<_> = self
            .variants
            .into_iter()
            .filter_map(|(selector, list)| {
                let models = stable_models(list.into_vec());
                let selected_model = models.first().map(|_| choose_weighted(&models))?;
                Some(BlockstateVariant {
                    id: format!("variant:{selector}"),
                    selector,
                    multipart_condition: None,
                    selected_model,
                    models,
                })
            })
            .collect();
        variants.extend(
            self.multipart
                .into_iter()
                .enumerate()
                .filter_map(|(index, part)| {
                    let models = stable_models(part.apply.into_vec());
                    let selected_model = models.first().map(|_| choose_weighted(&models))?;
                    Some(BlockstateVariant {
                        id: format!("multipart-{index:04}"),
                        selector: format!("multipart[{index}]"),
                        multipart_condition: part.when,
                        selected_model,
                        models,
                    })
                }),
        );
        variants
    }
}

fn parse_variant_selector(selector: &str) -> BTreeMap<String, Vec<String>> {
    selector
        .split(',')
        .filter_map(|property| {
            let (name, values) = property.trim().split_once('=')?;
            let values: Vec<_> = values
                .split('|')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect();
            (!name.is_empty() && !values.is_empty()).then(|| (name.to_owned(), values))
        })
        .collect()
}

fn collect_condition_properties(
    condition: &Value,
    properties: &mut BTreeMap<String, BTreeSet<String>>,
) {
    match condition {
        Value::Object(object) => {
            for (name, value) in object {
                if matches!(name.as_str(), "OR" | "AND") {
                    if let Some(conditions) = value.as_array() {
                        for nested in conditions {
                            collect_condition_properties(nested, properties);
                        }
                    }
                    continue;
                }
                let values = match value {
                    Value::String(values) => values
                        .split('|')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    Value::Bool(value) => vec![value.to_string()],
                    Value::Number(value) => vec![value.to_string()],
                    _ => Vec::new(),
                };
                if !values.is_empty() {
                    properties.entry(name.clone()).or_default().extend(values);
                }
            }
        }
        Value::Array(conditions) => {
            for nested in conditions {
                collect_condition_properties(nested, properties);
            }
        }
        _ => {}
    }
}

fn select_blockstate(
    properties: &BTreeMap<String, BTreeSet<String>>,
    selected_variant: Option<&BlockstateVariant>,
    requested: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let variant_values = selected_variant
        .map(|variant| parse_variant_selector(&variant.selector))
        .unwrap_or_default();
    properties
        .iter()
        .map(|(name, values)| {
            let selected = requested
                .get(name)
                .filter(|value| values.contains(*value))
                .or_else(|| {
                    variant_values
                        .get(name)
                        .and_then(|values| preferred_property_value(name, values))
                        .filter(|value| values.contains(*value))
                })
                .or_else(|| preferred_property_value(name, values))
                .expect("blockstate properties are collected from at least one value");
            (name.clone(), selected.to_owned())
        })
        .collect()
}

fn preferred_property_value<'a>(
    property: &str,
    values: impl IntoIterator<Item = &'a String>,
) -> Option<&'a String> {
    let values: Vec<&String> = values.into_iter().collect();
    // Runtime stateがないPreviewでは、単独設置に近いMinecraftの標準形を選ぶ。
    // wallのup=trueは中央postを出し、fence/paneの各方角falseは接続先のないarmを隠す。
    let preferred = match property {
        "up" => &["true"][..],
        "half" | "type" => &["bottom", "single"][..],
        "part" => &["lower"][..],
        "shape" => &["straight"][..],
        "facing" => &["north"][..],
        "axis" => &["y"][..],
        "face" => &["floor"][..],
        "age" | "power" | "rotation" => &["0"][..],
        _ => &[][..],
    };
    preferred
        .iter()
        .find_map(|preferred| {
            values
                .iter()
                .find(|value| value.as_str() == *preferred)
                .copied()
        })
        .or_else(|| {
            ["false", "none"].into_iter().find_map(|preferred| {
                values
                    .iter()
                    .find(|value| value.as_str() == preferred)
                    .copied()
            })
        })
        .or_else(|| values.into_iter().min())
}

fn multipart_matches(condition: Option<&Value>, state: &BTreeMap<String, String>) -> bool {
    let Some(condition) = condition else {
        return true;
    };
    match condition {
        Value::Object(object) => object.iter().all(|(name, expected)| match name.as_str() {
            "OR" => expected.as_array().is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|value| multipart_matches(Some(value), state))
            }),
            "AND" => expected.as_array().is_some_and(|conditions| {
                conditions
                    .iter()
                    .all(|value| multipart_matches(Some(value), state))
            }),
            property => {
                let Some(actual) = state.get(property) else {
                    return false;
                };
                match expected {
                    Value::String(values) => values
                        .split('|')
                        .map(str::trim)
                        .any(|value| value == actual),
                    Value::Bool(value) => actual == &value.to_string(),
                    Value::Number(value) => actual == &value.to_string(),
                    _ => false,
                }
            }
        }),
        Value::Array(conditions) => conditions
            .iter()
            .any(|value| multipart_matches(Some(value), state)),
        _ => false,
    }
}

fn stable_models(mut models: Vec<ModelReference>) -> Vec<ModelReference> {
    models.sort_by(|left, right| {
        left.model
            .cmp(&right.model)
            .then_with(|| left.x.cmp(&right.x))
            .then_with(|| left.y.cmp(&right.y))
            .then_with(|| left.uvlock.cmp(&right.uvlock))
            .then_with(|| right.weight.cmp(&left.weight))
    });
    models
}

fn choose_weighted(models: &[ModelReference]) -> ModelReference {
    models
        .iter()
        .min_by(|left, right| {
            right
                .weight
                .cmp(&left.weight)
                .then_with(|| left.model.cmp(&right.model))
        })
        .expect("blockstate variant must contain a model")
        .clone()
}

fn choose_variant<'a>(
    variants: &'a [BlockstateVariant],
    requested: Option<&str>,
) -> Option<&'a BlockstateVariant> {
    if let Some(requested) = requested
        && let Some(variant) = variants.iter().find(|variant| {
            variant.id.starts_with("variant:")
                && (variant.id == requested || variant.selector == requested)
        })
    {
        return Some(variant);
    }
    let candidates: Vec<&BlockstateVariant> = variants
        .iter()
        .filter(|variant| variant.id.starts_with("variant:"))
        .collect();
    if let Some(variant) = candidates
        .iter()
        .find(|variant| variant.selector.is_empty())
    {
        return Some(variant);
    }
    // 名前順の先頭を既定にすると、階段では "shape=inner_left" のような角の形が選ばれてしまう。
    // 優先順位（辞書式）:
    //   1. facing 以外の property が単独設置に近い値か（shape=straight, half=bottom など）
    //   2. blockstate の x / y 回転が無いか。vanilla はモデル本来の向き（インベントリ表示と同じ向き）を
    //      回転なしの variant に割り当てるため（階段は east、かまどは north）。facing の値を名前で
    //      決め打ちすると、階段の段差が裏を向いて箱のように見えてしまう
    //   3. facing の優先値 → 4. selector の名前順（決定的にするため）
    let mut properties = BTreeMap::<String, BTreeSet<String>>::new();
    for variant in &candidates {
        for (name, values) in parse_variant_selector(&variant.selector) {
            properties.entry(name).or_default().extend(values);
        }
    }
    let preferred: BTreeMap<&str, &String> = properties
        .iter()
        .filter_map(|(name, values)| {
            preferred_property_value(name, values).map(|value| (name.as_str(), value))
        })
        .collect();
    let matches = |variant: &BlockstateVariant, facing: bool| {
        parse_variant_selector(&variant.selector)
            .iter()
            .filter(|(name, _)| (name.as_str() == "facing") == facing)
            .filter(|(name, values)| {
                preferred
                    .get(name.as_str())
                    .is_some_and(|value| values.contains(value))
            })
            .count()
    };
    let unrotated = |variant: &BlockstateVariant| {
        variant.selected_model.x == 0 && variant.selected_model.y == 0
    };
    candidates.into_iter().max_by(|left, right| {
        matches(left, false)
            .cmp(&matches(right, false))
            .then_with(|| unrotated(left).cmp(&unrotated(right)))
            .then_with(|| matches(left, true).cmp(&matches(right, true)))
            // max_by は同点で後ろを返すため、名前順で先のものが勝つよう比較を反転する。
            .then_with(|| right.selector.cmp(&left.selector))
    })
}

fn has_requested_blockstate(
    properties: &BTreeMap<String, BTreeSet<String>>,
    requested: &BTreeMap<String, String>,
) -> bool {
    requested.iter().any(|(name, value)| {
        properties
            .get(name)
            .is_some_and(|values| values.contains(value))
    })
}

fn choose_variant_for_state<'a>(
    variants: &'a [BlockstateVariant],
    state: &BTreeMap<String, String>,
) -> Option<&'a BlockstateVariant> {
    variants
        .iter()
        .filter(|variant| variant.id.starts_with("variant:"))
        .filter(|variant| {
            let selector = parse_variant_selector(&variant.selector);
            !selector.is_empty()
                && selector.iter().all(|(name, accepted)| {
                    state
                        .get(name)
                        .is_some_and(|value| accepted.iter().any(|candidate| candidate == value))
                })
        })
        .min_by(|left, right| {
            let left_specificity = parse_variant_selector(&left.selector).len();
            let right_specificity = parse_variant_selector(&right.selector).len();
            right_specificity
                .cmp(&left_specificity)
                .then_with(|| left.selector.cmp(&right.selector))
        })
}

fn generated_layers(textures: &BTreeMap<String, TextureReference>) -> Vec<crate::ir::ItemLayer> {
    (0..)
        .take_while(|index| textures.contains_key(&format!("layer{index}")))
        .filter_map(|index| {
            textures
                .get(&format!("layer{index}"))
                .cloned()
                .map(|texture| crate::ir::ItemLayer {
                    index,
                    texture,
                    thickness: 1.0,
                })
        })
        .collect()
}
