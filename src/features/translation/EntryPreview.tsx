import { useCallback, useEffect, useMemo, useState } from "react";

import { getAssetBytes, getAssetPreviewManifest } from "../../lib/tauri";
import { Select } from "../../components/Controls";
import { useProjectStore } from "../../stores/projectStore";
import type {
  BlockPreviewManifest,
  EntityPreviewManifest,
  ItemPreviewManifest,
  PreviewManifest,
  PreviewOptions,
} from "../../types/preview";
import type { TranslationEntry } from "../../types/translation";
import { Preview } from "../preview/Preview";
import { getCachedPreviewTextureBytes } from "../preview/textureByteCache";
import { loadMinecraftJarState, useMinecraftJarStore } from "../preview/minecraftJarStore";

interface EntryPreviewProps {
  entry: TranslationEntry;
}

export function EntryPreview({ entry }: EntryPreviewProps) {
  const previewKey = isAssetTranslationKey(entry);
  const projectId = useProjectStore((state) => state.details?.project.id ?? "");
  const jar = useMinecraftJarStore();
  useEffect(() => {
    if (previewKey) void loadMinecraftJarState();
  }, [previewKey]);
  const [selection, setSelection] = useState<{ entryId: number; options: PreviewOptions } | null>(null);
  const options = useMemo(
    () => selection?.entryId === entry.id ? selection.options : {},
    [entry.id, selection],
  );
  const [result, setResult] = useState<{
    entryId: number;
    jarVersion: number;
    projectId: string;
    options: PreviewOptions;
    manifest: PreviewManifest | null;
    error: boolean;
  }>({ entryId: entry.id, jarVersion: -1, projectId, options: {}, manifest: null, error: false });
  const getModelJson = useCallback(async (source: { sourceId: string; archivePath: string }) => {
    const bytes = await getAssetBytes(source.sourceId, source.archivePath);
    return JSON.parse(new TextDecoder().decode(bytes)) as unknown;
  }, []);
  const getTextureBytes = useCallback(
    (path: string) => getCachedPreviewTextureBytes(path, JSON.stringify([projectId, entry.modUnitId ?? entry.id])),
    [projectId, entry.modUnitId, entry.id],
  );

  useEffect(() => {
    if (!previewKey || jar.path === undefined) return;
    let active = true;
    // 高速な矢印移動では最後に止まった行だけmanifestを取りに行き、古い結果も表示しない。
    const timeout = window.setTimeout(() => {
      void getAssetPreviewManifest(entry.id, options)
        .then((manifest) => {
          if (active && useMinecraftJarStore.getState().version === jar.version) setResult({ entryId: entry.id, jarVersion: jar.version, projectId, options, manifest, error: false });
        })
        .catch(() => {
          if (active && useMinecraftJarStore.getState().version === jar.version) setResult({ entryId: entry.id, jarVersion: jar.version, projectId, options, manifest: null, error: true });
        });
    }, 90);
    return () => {
      active = false;
      window.clearTimeout(timeout);
    };
  }, [entry.id, entry.key, entry.namespace, options, previewKey, jar.path, jar.version, projectId]);

  // 表示可否は flags ではなく resolver の結果で決める（理由は preview/thumbnails.ts の isPreviewableEntry）。
  if (!previewKey) return <PreviewEmpty />;

  if (jar.error) return <PreviewEmpty label="読み込めません" />;
  const matchesRequest = result.entryId === entry.id && result.options === options
    && result.jarVersion === jar.version && result.projectId === projectId;
  const manifest = matchesRequest ? result.manifest : null;
  if (manifest && isEmptyPreview(manifest)) return <PreviewEmpty />;
  if (matchesRequest && result.error) return <PreviewEmpty label="読み込めません" />;
  if (!manifest) {
    return <div className="preview__loading" aria-label="プレビュー">読み込み中</div>;
  }

  return (
    <div className="editor-entry-preview">
      <Preview
        manifest={manifest}
        getTextureBytes={getTextureBytes}
        getModelJson={getModelJson}
      />
      <PreviewVariantSelector
        manifest={manifest}
        options={options}
        onChange={(nextOptions) => setSelection({ entryId: entry.id, options: nextOptions })}
      />
    </div>
  );
}

function PreviewEmpty({ label = "プレビューなし" }: { label?: string }) {
  return (
    <div
      className="editor-inspector__preview editor-inspector__preview--empty"
      aria-label="プレビュー"
    >
      {label}
    </div>
  );
}

function PreviewVariantSelector({
  manifest,
  options,
  onChange,
}: {
  manifest: PreviewManifest;
  options: PreviewOptions;
  onChange: (options: PreviewOptions) => void;
}) {
  if (manifest.kind === "block") {
    const block = manifest.manifest as BlockPreviewManifest;
    return (
      <div className="preview__variant-list">
        {block.properties.map((property) => (
          <div key={property.name}>
            <span>{propertyLabel(property.name)}</span>
            <Select
              aria-label={propertyLabel(property.name)}
              className="preview__variant-select"
              options={property.values.map((value) => ({ value, label: propertyValueLabel(value, property.name) }))}
              value={options.blockstate?.[property.name] ?? property.selectedValue}
              onChange={(value) => onChange({
                ...options,
                selectedVariant: null,
                blockstate: { ...options.blockstate, [property.name]: value },
              })}
            />
          </div>
        ))}
        {block.properties.length === 0 && block.variants.length > 1 && (
          <div>
            <span>状態</span>
            <Select
              aria-label="状態"
              className="preview__variant-select"
              options={block.variants.map((variant) => ({
                value: variant.id,
                label: variantSelectorLabel(variant.selector, variant.id),
              }))}
              value={options.selectedVariant ?? block.selectedVariant ?? block.variants[0].id}
              onChange={(value) => onChange({ ...options, selectedVariant: value })}
            />
          </div>
        )}
      </div>
    );
  }
  if (manifest.kind === "item") {
    const item = manifest.manifest as ItemPreviewManifest;
    if (item.variants.length === 0) return null;
    return (
      <div className="preview__variant-list">
        {item.variants.map((variant) => (
          <div key={variant.selector}>
            <span>{propertyLabel(variant.property)}</span>
            <Select
              aria-label={propertyLabel(variant.property)}
              className="preview__variant-select"
              options={variant.options.map((option) => ({ value: option.id, label: itemOptionLabel(option) }))}
              value={options.item?.[variant.selector] ?? variant.selected}
              onChange={(value) => onChange({
                ...options,
                item: { ...options.item, [variant.selector]: value },
              })}
            />
          </div>
        ))}
      </div>
    );
  }
  if (manifest.kind === "entity") {
    const entity = manifest.manifest as EntityPreviewManifest;
    if (entity.textureCandidates.length === 0 && entity.confidence === "high") return null;
    const selected = options.selectedTexture ?? (entity.texture?.path
      ? `${entity.texture.path.sourceId}:${entity.texture.path.archivePath}`
      : entity.textureCandidates[0] ? candidateKey(entity.textureCandidates[0]) : undefined);
    return (
      <div className="preview__variant-list">
        {entity.confidence !== "high" && <p>候補: {entity.geometry.identifier}</p>}
        {entity.textureCandidates.length > 0 && (
          <div>
            <span>テクスチャ</span>
            <Select
              aria-label="テクスチャ候補"
              className="preview__variant-select"
              options={entity.textureCandidates.map((candidate) => ({
                value: candidateKey(candidate),
                label: candidate.confidence === "high" ? candidate.asset.resourceId : `候補: ${candidate.asset.resourceId}`,
              }))}
              value={selected ?? ""}
              onChange={(value) => onChange({ ...options, selectedTexture: value })}
            />
          </div>
        )}
      </div>
    );
  }
  return null;
}

function candidateKey(candidate: EntityPreviewManifest["textureCandidates"][number]) {
  return `${candidate.asset.sourceId}:${candidate.asset.archivePath}`;
}

function itemOptionLabel(option: ItemPreviewManifest["variants"][number]["options"][number]) {
  if (option.fallback) return "既定";
  if (typeof option.value === "string") return propertyValueLabel(option.value);
  if (typeof option.value === "boolean") return propertyValueLabel(String(option.value));
  if (option.value !== null) return JSON.stringify(option.value);
  return option.id;
}

const PROPERTY_LABELS: Record<string, string> = {
  age: "成長段階",
  attachment: "取り付け位置",
  attached: "接続状態",
  axis: "軸",
  face: "設置面",
  facing: "向き",
  facing_direction: "向き",
  half: "上下",
  hinge: "開く向き",
  layers: "層",
  level: "段階",
  lit: "点灯状態",
  mode: "モード",
  moisture: "湿り気",
  open: "開閉状態",
  orientation: "向き",
  part: "部位",
  powered: "作動状態",
  rotation: "回転",
  shape: "形",
  snowy: "積雪状態",
  type: "種類",
  waterlogged: "水没状態",
};

const VALUE_LABELS: Record<string, string> = {
  ascending_east: "東上り",
  ascending_north: "北上り",
  ascending_south: "南上り",
  ascending_west: "西上り",
  bottom: "下",
  ceiling: "天井",
  down: "下",
  east: "東",
  false: "なし",
  floor: "床",
  horizontal: "水平",
  inner_left: "内側・左",
  inner_right: "内側・右",
  left: "左",
  low: "低",
  north: "北",
  north_east: "北東",
  north_south: "南北",
  north_west: "北西",
  off: "オフ",
  on: "オン",
  outer_left: "外側・左",
  outer_right: "外側・右",
  right: "右",
  single: "単体",
  south: "南",
  south_east: "南東",
  east_west: "東西",
  south_west: "南西",
  straight: "まっすぐ",
  tall: "高",
  top: "上",
  true: "あり",
  up: "上",
  vertical: "垂直",
  wall: "壁",
  west: "西",
};

const PROPERTY_VALUE_LABELS: Record<string, Record<string, string>> = {
  lit: { false: "消灯", true: "点灯" },
  open: { false: "閉", true: "開" },
};

function propertyLabel(property: string) {
  return PROPERTY_LABELS[property] ?? property;
}

function propertyValueLabel(value: string, property?: string) {
  const propertyLabel = property ? PROPERTY_VALUE_LABELS[property]?.[value] : undefined;
  if (propertyLabel) return propertyLabel;
  if (property === "axis" && /^[xyz]$/i.test(value)) return `${value.toUpperCase()}軸`;
  return VALUE_LABELS[value] ?? value;
}

function variantSelectorLabel(selector: string, id: string) {
  const label = selector || id;
  return label.split(",").map((part) => {
    const separator = part.indexOf("=");
    if (separator < 0) return propertyValueLabel(part.trim());
    const property = part.slice(0, separator).trim();
    const value = part.slice(separator + 1).trim();
    return `${propertyLabel(property)}=${propertyValueLabel(value, property)}`;
  }).join(" / ");
}

export function entryHasPreviewKey(entry: TranslationEntry) {
  return ["block", "item", "entity"].some((kind) => entry.key.startsWith(`${kind}.${entry.namespace}.`));
}

function isAssetTranslationKey(entry: TranslationEntry) {
  return entryHasPreviewKey(entry);
}

function isEmptyPreview(manifest: PreviewManifest) {
  if (manifest.kind !== "noPreview") return false;
  const content = manifest.manifest as { textureCandidates?: unknown[]; rawModelInfo?: unknown };
  return (!content.textureCandidates || content.textureCandidates.length === 0) && content.rawModelInfo == null;
}
