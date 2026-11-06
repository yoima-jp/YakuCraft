import { useCallback, useEffect, useMemo, useState } from "react";

import { getAssetBytes, getAssetPreviewManifest } from "../../lib/tauri";
import { Select } from "../../components/Controls";
import { useT, useUiLanguage } from "../../i18n";
import { minecraftLocaleToBcp47 } from "../../i18n/minecraftLanguages";
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
  const t = useT();
  const uiLanguage = useUiLanguage();
  const previewKey = isAssetTranslationKey(entry);
  const projectId = useProjectStore((state) => state.details?.project.id ?? "");
  const targetLocale = useProjectStore((state) => state.details?.targetLocale ?? "");
  const targetLanguage = minecraftLocaleToBcp47(targetLocale);
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
  if (!previewKey) return <PreviewEmpty lang={uiLanguage} />;

  if (jar.error) return <PreviewEmpty lang={uiLanguage} label={t("entryPreview.loadFailed")} />;
  const matchesRequest = result.entryId === entry.id && result.options === options
    && result.jarVersion === jar.version && result.projectId === projectId;
  const manifest = matchesRequest ? result.manifest : null;
  if (manifest && isEmptyPreview(manifest)) return <PreviewEmpty lang={uiLanguage} />;
  if (matchesRequest && result.error) return <PreviewEmpty lang={uiLanguage} label={t("entryPreview.loadFailed")} />;
  if (!manifest) {
    return (
      <div className="preview__loading" lang={uiLanguage} dir="auto" aria-label={t("entryPreview.preview")}>
        {t("entryPreview.loading")}
      </div>
    );
  }

  return (
    <div className="editor-entry-preview" lang={uiLanguage} dir="auto">
      <Preview
        manifest={manifest}
        getTextureBytes={getTextureBytes}
        getModelJson={getModelJson}
        contentLang={targetLanguage}
      />
      <PreviewVariantSelector
        manifest={manifest}
        options={options}
        onChange={(nextOptions) => setSelection({ entryId: entry.id, options: nextOptions })}
      />
    </div>
  );
}

function PreviewEmpty({ label, lang }: { label?: string; lang?: string }) {
  const t = useT();
  return (
    <div
      className="editor-inspector__preview editor-inspector__preview--empty"
      lang={lang}
      dir="auto"
      aria-label={t("entryPreview.preview")}
    >
      {label ?? t("entryPreview.empty")}
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
  const t = useT();
  if (manifest.kind === "block") {
    const block = manifest.manifest as BlockPreviewManifest;
    return (
      <div className="preview__variant-list">
        {block.properties.map((property) => (
          <div key={property.name}>
            <span>{propertyLabel(property.name, t)}</span>
            <Select
              aria-label={propertyLabel(property.name, t)}
              className="preview__variant-select"
              options={property.values.map((value) => ({ value, label: propertyValueLabel(value, property.name, t) }))}
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
            <span>{t("entryPreview.state")}</span>
            <Select
              aria-label={t("entryPreview.state")}
              className="preview__variant-select"
              options={block.variants.map((variant) => ({
                value: variant.id,
                label: variantSelectorLabel(variant.selector, variant.id, t),
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
            <span>{propertyLabel(variant.property, t)}</span>
            <Select
              aria-label={propertyLabel(variant.property, t)}
              className="preview__variant-select"
              options={variant.options.map((option) => ({ value: option.id, label: itemOptionLabel(option, t) }))}
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
        {entity.confidence !== "high" && <p>{t("entryPreview.candidate", { value: entity.geometry.identifier })}</p>}
        {entity.textureCandidates.length > 0 && (
          <div>
            <span>{t("entryPreview.texture")}</span>
            <Select
              aria-label={t("entryPreview.textureCandidate")}
              className="preview__variant-select"
              options={entity.textureCandidates.map((candidate) => ({
                value: candidateKey(candidate),
                label: candidate.confidence === "high"
                  ? candidate.asset.resourceId
                  : t("entryPreview.candidate", { value: candidate.asset.resourceId }),
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

type PreviewTranslator = ReturnType<typeof useT>;

function itemOptionLabel(option: ItemPreviewManifest["variants"][number]["options"][number], t: PreviewTranslator) {
  if (option.fallback) return t("entryPreview.fallback");
  if (typeof option.value === "string") return propertyValueLabel(option.value, undefined, t);
  if (typeof option.value === "boolean") return propertyValueLabel(String(option.value), undefined, t);
  if (option.value !== null) return JSON.stringify(option.value);
  return option.id;
}

const PROPERTY_LABELS = {
  age: "entryPreview.property.age",
  attachment: "entryPreview.property.attachment",
  attached: "entryPreview.property.attached",
  axis: "entryPreview.property.axis",
  face: "entryPreview.property.face",
  facing: "entryPreview.property.facing",
  facing_direction: "entryPreview.property.facing",
  half: "entryPreview.property.half",
  hinge: "entryPreview.property.hinge",
  layers: "entryPreview.property.layers",
  level: "entryPreview.property.level",
  lit: "entryPreview.property.lit",
  mode: "entryPreview.property.mode",
  moisture: "entryPreview.property.moisture",
  open: "entryPreview.property.open",
  orientation: "entryPreview.property.orientation",
  part: "entryPreview.property.part",
  powered: "entryPreview.property.powered",
  rotation: "entryPreview.property.rotation",
  shape: "entryPreview.property.shape",
  snowy: "entryPreview.property.snowy",
  type: "entryPreview.property.type",
  waterlogged: "entryPreview.property.waterlogged",
} as const;

const VALUE_LABELS = {
  ascending_east: "entryPreview.value.ascendingEast",
  ascending_north: "entryPreview.value.ascendingNorth",
  ascending_south: "entryPreview.value.ascendingSouth",
  ascending_west: "entryPreview.value.ascendingWest",
  bottom: "entryPreview.value.bottom",
  ceiling: "entryPreview.value.ceiling",
  down: "entryPreview.value.down",
  east: "entryPreview.value.east",
  false: "entryPreview.value.false",
  floor: "entryPreview.value.floor",
  horizontal: "entryPreview.value.horizontal",
  inner_left: "entryPreview.value.innerLeft",
  inner_right: "entryPreview.value.innerRight",
  left: "entryPreview.value.left",
  low: "entryPreview.value.low",
  north: "entryPreview.value.north",
  north_east: "entryPreview.value.northEast",
  north_south: "entryPreview.value.northSouth",
  north_west: "entryPreview.value.northWest",
  off: "entryPreview.value.off",
  on: "entryPreview.value.on",
  outer_left: "entryPreview.value.outerLeft",
  outer_right: "entryPreview.value.outerRight",
  right: "entryPreview.value.right",
  single: "entryPreview.value.single",
  south: "entryPreview.value.south",
  south_east: "entryPreview.value.southEast",
  east_west: "entryPreview.value.eastWest",
  south_west: "entryPreview.value.southWest",
  straight: "entryPreview.value.straight",
  tall: "entryPreview.value.tall",
  top: "entryPreview.value.top",
  true: "entryPreview.value.true",
  up: "entryPreview.value.up",
  vertical: "entryPreview.value.vertical",
  wall: "entryPreview.value.wall",
  west: "entryPreview.value.west",
} as const;

const PROPERTY_VALUE_LABELS = {
  lit: { false: "entryPreview.value.litFalse", true: "entryPreview.value.litTrue" },
  open: { false: "entryPreview.value.openFalse", true: "entryPreview.value.openTrue" },
} as const;

function propertyLabel(property: string, t: PreviewTranslator) {
  const key = PROPERTY_LABELS[property as keyof typeof PROPERTY_LABELS];
  return key ? t(key) : property;
}

function propertyValueLabel(value: string, property: string | undefined, t: PreviewTranslator) {
  const propertyValueKey = property === "lit" || property === "open"
    ? PROPERTY_VALUE_LABELS[property][value as "false" | "true"]
    : undefined;
  if (propertyValueKey) return t(propertyValueKey);
  if (property === "axis" && /^[xyz]$/i.test(value)) return t("entryPreview.axis", { axis: value.toUpperCase() });
  const valueKey = VALUE_LABELS[value as keyof typeof VALUE_LABELS];
  return valueKey ? t(valueKey) : value;
}

function variantSelectorLabel(selector: string, id: string, t: PreviewTranslator) {
  const label = selector || id;
  return label.split(",").map((part) => {
    const separator = part.indexOf("=");
    if (separator < 0) return propertyValueLabel(part.trim(), undefined, t);
    const property = part.slice(0, separator).trim();
    const value = part.slice(separator + 1).trim();
    return `${propertyLabel(property, t)}=${propertyValueLabel(value, property, t)}`;
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
