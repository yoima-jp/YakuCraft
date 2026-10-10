/** yc-assets の PreviewManifest を serde の camelCase 出力に合わせた型。 */
export type Tuple3 = [number, number, number];
export type Tuple4 = [number, number, number, number];
export type Tuple2 = [number, number];

export type AssetType =
  | "blockstate"
  | "block_model"
  | "item_model"
  | "item_definition"
  | "texture_block"
  | "texture_item"
  | "texture_entity"
  | "geo_model"
  | "animation"
  | "other_model";

export type SourceKind = "mod" | "minecraft" | "builtin";
export type LinkConfidence = "high" | "medium" | "low";
export type FaceName = "down" | "up" | "north" | "south" | "west" | "east";
export type FaceAxis = "x" | "y" | "z";

export interface AssetLocation {
  sourceId: string;
  sourceKind: SourceKind;
  archivePath: string;
  namespace: string;
  resourceId: string;
  assetType: AssetType;
}

export interface ElementRotation {
  origin: Tuple3;
  axis: FaceAxis;
  angle: number;
  rescale: boolean;
}

export interface ResolvedFace {
  texture: string;
  texturePath: AssetLocation | null;
  uv: Tuple4;
  rotation: number;
  cullface: FaceName | null;
  tintindex: number | null;
  missing: boolean;
  searchedSources: string[];
}

export interface ResolvedElement {
  from: Tuple3;
  to: Tuple3;
  rotation?: ElementRotation;
  shade: boolean;
  faces: Partial<Record<FaceName, ResolvedFace>>;
}

export interface DisplayTransform {
  rotation: Tuple3;
  translation: Tuple3;
  scale: Tuple3;
}

export interface TextureReference {
  location: string;
  path: AssetLocation | null;
  missing: boolean;
  searchedSources: string[];
}

export interface ResolvedModel {
  model: string;
  source: AssetLocation | null;
  sourceChain: AssetLocation[];
  textures: Record<string, TextureReference>;
  elements: ResolvedElement[];
  ambientOcclusion: boolean;
  display: Record<string, DisplayTransform>;
  guiLight: string | null;
  generatedItem: boolean;
  unsupportedFeatures?: string[];
}

export interface ModelReference {
  model: string;
  x: number;
  y: number;
  uvlock: boolean;
  weight: number;
}

export interface BlockstateVariant {
  id: string;
  selector: string;
  multipartCondition: unknown | null;
  models: ModelReference[];
  selectedModel: ModelReference;
}

export interface ResolvedBlockModel {
  model: ResolvedModel;
  xRotation: number;
  yRotation: number;
  uvlock: boolean;
}

export interface BlockPreviewManifest {
  translationKey: string;
  variants: BlockstateVariant[];
  selectedVariant: string | null;
  properties: BlockstateProperty[];
  selectedState: Record<string, string>;
  models: ResolvedBlockModel[];
}

export interface BlockstateProperty {
  name: string;
  values: string[];
  selectedValue: string;
}

export interface ItemLayer {
  index: number;
  texture: TextureReference;
  /** 厚みはMinecraftの16px座標系。 */
  thickness: number;
}

export interface ItemPreviewManifest {
  translationKey: string;
  model: ResolvedModel;
  models: ResolvedModel[];
  layers: ItemLayer[];
  definition: ResolvedItemDefinition | null;
  variants: ItemDefinitionVariant[];
}

export type ResolvedItemDefinition =
  | { kind: "model"; modelIndex: number; tints: unknown[] }
  | { kind: "composite"; children: ResolvedItemDefinition[] }
  | { kind: "condition" | "select" | "rangeDispatch"; property: string; selector: string; selected: string; child: ResolvedItemDefinition }
  | { kind: "empty" };

export interface ItemDefinitionVariant {
  selector: string;
  property: string;
  selected: string;
  options: Array<{ id: string; value: unknown | null; fallback: boolean }>;
}

export interface TextureCandidate {
  asset: AssetLocation;
  confidence: LinkConfidence;
  confirmed: boolean;
}

export interface TexturePreviewManifest {
  translationKey: string;
  textures: TextureReference[];
  candidates: TextureCandidate[];
  reason?: string;
  rawModelInfo: unknown | null;
}

export interface EntityPreviewManifest {
  translationKey: string;
  geometry: EntityGeometry;
  geometrySource: AssetLocation;
  confidence: LinkConfidence;
  texture?: TextureReference;
  textureConfidence?: LinkConfidence;
  textureCandidates: TextureCandidate[];
}

export interface EntityGeometry {
  identifier: string;
  formatVersion: string;
  textureWidth: number | null;
  textureHeight: number | null;
  bones: EntityBone[];
}

export interface EntityBone {
  name: string;
  parent?: string;
  pivot?: Tuple3;
  rotation?: Tuple3;
  cubes: EntityCube[];
}

export interface EntityCube {
  origin: Tuple3;
  size: Tuple3;
  pivot?: Tuple3;
  rotation?: Tuple3;
  uv?: EntityCubeUv;
  inflate: number;
  mirror: boolean;
}

export type EntityCubeUv =
  | { kind: "box"; value: Tuple2 }
  | { kind: "perFace"; value: Partial<Record<FaceName, EntityFaceUv>> };

export interface EntityFaceUv {
  uv: Tuple2;
  uvSize: Tuple2;
}

export interface NoPreviewManifest {
  translationKey: string;
  reason: string;
  textureCandidates: TextureCandidate[];
  rawModelInfo: unknown | null;
  rawModelPath: string | null;
}

/** Rust側でvariantが増えてもフロントの入力境界で安全に扱うための開いたunion。 */
export type KnownPreviewManifest =
  | { kind: "block"; manifest: BlockPreviewManifest }
  | { kind: "item"; manifest: ItemPreviewManifest }
  | { kind: "entity"; manifest: EntityPreviewManifest }
  | { kind: "texture"; manifest: TexturePreviewManifest }
  | { kind: "noPreview"; manifest: NoPreviewManifest };

export type UnknownPreviewManifest = { kind: string; manifest: unknown };
export type PreviewManifest = KnownPreviewManifest | UnknownPreviewManifest;

export type PreviewMode = "3d" | "texture" | "model";
export type PreviewTextureLoader = (texturePath: string) => Promise<Uint8Array>;
export interface PreviewOptions {
  selectedVariant?: string | null;
  blockstate?: Record<string, string>;
  item?: Record<string, string>;
  selectedTexture?: string | null;
}

export interface MinecraftJarCandidate {
  version: string;
  path: string;
}

export type PreviewKind = "block" | "item" | "entity" | "texture" | "noPreview" | "unknown";

/** API/将来のRust variantをそのまま信頼せず、描画前にkindだけを確定する。 */
export function classifyPreviewManifest(value: unknown): PreviewKind {
  if (!value || typeof value !== "object" || !("kind" in value) || !("manifest" in value)) {
    return "unknown";
  }

  const candidate = value as { kind: unknown; manifest: unknown };
  if (!candidate.manifest || typeof candidate.manifest !== "object") return "unknown";

  switch (candidate.kind) {
    case "block":
    case "item":
    case "entity":
    case "texture":
    case "noPreview":
      return candidate.kind;
    default:
      return "unknown";
  }
}

/** 未知variantは画面上で必ずNo Previewへ収束させる。 */
export function previewKindOrNoPreview(value: unknown): Exclude<PreviewKind, "unknown"> {
  const kind = classifyPreviewManifest(value);
  return kind === "unknown" ? "noPreview" : kind;
}

/** Source間で同じarchive pathが衝突しないよう、binary取得用のkeyを安定して作る。 */
export function textureFetchPath(location: AssetLocation | null, fallback: string): string {
  return location
    ? `${encodeURIComponent(location.sourceId)}:${encodeURIComponent(location.archivePath)}`
    : fallback;
}
