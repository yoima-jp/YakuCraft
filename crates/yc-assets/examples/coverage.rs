use std::{
    collections::{BTreeMap, HashMap},
    env,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use yc_archive::{Archive, EntryKind};
use yc_assets::{
    AssetPack, AssetResolver, AssetSource, AssetType, PreviewManifest, ResolvedModel, SourceKind,
};

struct JarAssets {
    archive: Mutex<Archive<File>>,
    paths: Vec<String>,
}

impl JarAssets {
    fn open(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let archive = Archive::open(path)?;
        let paths = archive
            .list_entries()
            .iter()
            .filter(|entry| !entry.is_directory && entry.exclusion_reasons.is_empty())
            .filter_map(|entry| entry.normalized_path.as_deref().map(str::to_owned))
            .collect();
        Ok(Self {
            archive: Mutex::new(archive),
            paths,
        })
    }
}

impl AssetSource for JarAssets {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let mut archive = self
            .archive
            .lock()
            .map_err(|_| "JAR reader lock was poisoned".to_owned())?;
        let kind = if path.ends_with(".png") {
            EntryKind::Texture
        } else {
            EntryKind::ModelJson
        };
        archive
            .read_entry(path, kind)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

#[derive(Default)]
struct Counts {
    total: usize,
    three_d: usize,
    texture: usize,
    unresolved: usize,
    reasons: BTreeMap<String, usize>,
    reason_examples: BTreeMap<String, String>,
    limitations: BTreeMap<String, usize>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mods = Vec::new();
    let mut minecraft = None;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--minecraft" {
            minecraft = Some(PathBuf::from(
                args.next()
                    .ok_or("--minecraft requires a vanilla JAR path")?,
            ));
        } else if argument == "--help" || argument == "-h" {
            print_usage();
            return Ok(());
        } else if argument.starts_with('-') {
            return Err(format!("unknown option: {argument}").into());
        } else {
            mods.push(PathBuf::from(argument));
        }
    }
    if mods.is_empty() && minecraft.is_none() {
        print_usage();
        return Err("provide a mod JAR, --minecraft JAR, or both".into());
    }

    let mut packs = Vec::new();
    let mut readers = HashMap::<String, Arc<JarAssets>>::new();
    for (index, path) in mods.iter().enumerate() {
        let id = format!("mod{}:{}", index + 1, jar_name(path));
        let source = Arc::new(JarAssets::open(path)?);
        readers.insert(id.clone(), source.clone());
        packs.push(AssetPack::new(id, SourceKind::Mod, source));
    }
    if let Some(path) = minecraft.as_deref() {
        let id = format!("minecraft:{}", jar_name(path));
        let source = Arc::new(JarAssets::open(path)?);
        readers.insert(id.clone(), source.clone());
        packs.push(AssetPack::new(id, SourceKind::Minecraft, source));
    }

    let resolver = AssetResolver::new(packs);
    let mut counts = BTreeMap::<(String, &'static str), Counts>::new();
    for asset in resolver.index().assets() {
        if asset.source_kind == SourceKind::Builtin {
            continue;
        }
        let (kind, key) = match asset.asset_type {
            AssetType::Blockstate => (
                "blockstate",
                format!("block.{}.{}", asset.namespace, asset.resource_id),
            ),
            AssetType::ItemModel => {
                let id = asset
                    .resource_id
                    .strip_prefix("item/")
                    .unwrap_or(&asset.resource_id);
                ("item model", format!("item.{}.{}", asset.namespace, id))
            }
            AssetType::ItemDefinition => (
                "item definition",
                format!("item.{}.{}", asset.namespace, asset.resource_id),
            ),
            _ => continue,
        };

        let result = if asset.asset_type == AssetType::ItemModel {
            let model = format!("{}:{}", asset.namespace, asset.resource_id);
            classify_item_model(
                resolver.resolve_model(&model),
                readers.get(&asset.source_id),
                &asset.archive_path,
            )
        } else {
            classify(resolver.resolve_preview(&key, None))
        };
        let entry = counts.entry((asset.source_id.clone(), kind)).or_default();
        entry.total += 1;
        match result {
            Coverage::ThreeD(limitations) => {
                entry.three_d += 1;
                for limitation in limitations {
                    *entry.limitations.entry(limitation).or_default() += 1;
                }
            }
            Coverage::Texture(reason) => {
                entry.texture += 1;
                record_reason(entry, reason, &key);
            }
            Coverage::Unresolved(reason) => {
                entry.unresolved += 1;
                record_reason(entry, reason, &key);
            }
        }
    }

    println!("source\ttype\ttotal\t3d\ttexture fallback\tunresolved\t3d %");
    for ((source, kind), count) in counts {
        let percent = ratio(count.three_d, count.total);
        println!(
            "{source}\t{kind}\t{}\t{}\t{}\t{}\t{percent:.1}%",
            count.total, count.three_d, count.texture, count.unresolved,
        );
        for (reason, amount) in count.reasons {
            let example = count
                .reason_examples
                .get(&reason)
                .map(String::as_str)
                .unwrap_or("unknown asset");
            println!("  reason\t{amount}\t{reason}\t{example}");
        }
        for (limitation, amount) in count.limitations {
            println!("  3d limitation\t{amount}\t{limitation}");
        }
    }
    Ok(())
}

fn record_reason(count: &mut Counts, reason: String, asset_key: &str) {
    *count.reasons.entry(reason.clone()).or_default() += 1;
    count
        .reason_examples
        .entry(reason)
        .or_insert_with(|| asset_key.to_owned());
}

enum Coverage {
    ThreeD(Vec<String>),
    Texture(String),
    Unresolved(String),
}

fn classify(manifest: PreviewManifest) -> Coverage {
    match manifest {
        PreviewManifest::Block(block) => {
            if block
                .models
                .iter()
                .any(|model| model_is_drawable(&model.model))
            {
                Coverage::ThreeD(
                    block
                        .models
                        .iter()
                        .flat_map(|model| model.model.unsupported_features.iter().cloned())
                        .collect(),
                )
            } else if block
                .models
                .iter()
                .any(|model| !model.model.textures.is_empty())
            {
                Coverage::Texture(format!(
                    "no geometry: {}",
                    block
                        .models
                        .iter()
                        .map(|model| model.model.model.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            } else {
                Coverage::Unresolved(format!(
                    "no geometry or textures: {}",
                    block
                        .models
                        .iter()
                        .map(|model| model.model.model.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
        PreviewManifest::Item(item) => {
            if !item.layers.is_empty() || item.models.iter().any(model_is_drawable) {
                Coverage::ThreeD(
                    item.models
                        .iter()
                        .flat_map(|model| model.unsupported_features.iter().cloned())
                        .collect(),
                )
            } else if item.models.iter().any(|model| !model.textures.is_empty()) {
                Coverage::Texture(format!(
                    "no geometry: {}",
                    item.models
                        .iter()
                        .map(|model| model.model.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            } else {
                Coverage::Unresolved("item definition has no renderable model".to_owned())
            }
        }
        PreviewManifest::Texture(texture) => {
            if texture.textures.is_empty() && texture.candidates.is_empty() {
                Coverage::Unresolved("texture fallback has no candidates".to_owned())
            } else {
                Coverage::Texture(
                    texture
                        .reason
                        .unwrap_or_else(|| "texture candidates only".to_owned()),
                )
            }
        }
        PreviewManifest::NoPreview(preview) => {
            if preview.texture_candidates.is_empty() {
                Coverage::Unresolved(preview.reason)
            } else {
                Coverage::Texture(preview.reason)
            }
        }
        PreviewManifest::Entity(_) => {
            Coverage::Texture("entity preview is outside block and item coverage".to_owned())
        }
    }
}

fn classify_item_model(
    result: Result<ResolvedModel, yc_assets::ResolverError>,
    source: Option<&Arc<JarAssets>>,
    archive_path: &str,
) -> Coverage {
    match result {
        Ok(model) => {
            if model_is_drawable(&model)
                || (model.generated_item && model.textures.contains_key("layer0"))
            {
                Coverage::ThreeD(model.unsupported_features)
            } else if !model.textures.is_empty() {
                Coverage::Texture(format!("no geometry in {}", model.model))
            } else {
                Coverage::Unresolved(format!("no geometry or textures in {}", model.model))
            }
        }
        Err(error) => {
            let has_texture = source
                .and_then(|source| source.read(archive_path).ok().flatten())
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|value| {
                    value
                        .get("textures")
                        .and_then(serde_json::Value::as_object)
                        .cloned()
                })
                .is_some_and(|textures| !textures.is_empty());
            if has_texture {
                Coverage::Texture(error.to_string())
            } else {
                Coverage::Unresolved(error.to_string())
            }
        }
    }
}

fn model_is_drawable(model: &ResolvedModel) -> bool {
    model
        .elements
        .iter()
        .any(|element| !element.faces.is_empty())
}

fn ratio(part: usize, total: usize) -> f64 {
    if total == 0 {
        100.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

fn jar_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("archive")
        .to_owned()
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p yc-assets --example coverage -- [<mod.jar> ...] [--minecraft <vanilla.jar>]"
    );
}
