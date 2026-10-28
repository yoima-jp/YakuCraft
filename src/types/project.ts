import type { EntryFlag, TranslationOrigin } from "./translation";

export interface MinecraftVersionDetectionDto {
  version: string;
  method: "metadata" | "packMcmeta" | "resourceStructure";
  originalValue: string | null;
  rounded: boolean;
}

export interface ProjectSummaryDto {
  id: string;
  displayName: string;
  modCount: number;
  instancePath: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface ModUnitRemovalInfoDto {
  hasUnrestoredReplacements: boolean;
  hasUnrestoredFileDeployments: boolean;
}

export interface ModSummaryDto {
  modUnitId: number;
  modId: string | null;
  name: string | null;
  version: string | null;
  loader: string;
  minecraftVersion: string | null;
}

export interface NamespaceSummaryDto {
  modUnitId: number;
  namespace: string;
  containerPath: string;
  included: boolean;
  entryCount: number;
}

export interface LanguageSummaryDto {
  modUnitId: number;
  namespace: string;
  locale: string;
  format: "json" | "legacyLang";
  entryCount: number;
  containerPath: string;
  archivePath: string;
}

export interface NestedArchiveSummaryDto {
  modUnitId: number;
  archivePath: string;
  modId: string | null;
  name: string | null;
  version: string | null;
  hasLanguages: boolean;
  skippedReason: string | null;
}

export interface ProjectCountsDto {
  total: number;
  translated: number;
  missing: number;
  needsReview: number;
}

export interface ProjectAssetSummaryDto {
  total: number;
  blockModels: number;
  itemModels: number;
  entityTextures: number;
  geometryModels: number;
  unresolvedEntries: number;
}

export interface ProjectDetailsDto {
  project: ProjectSummaryDto;
  mods: ModSummaryDto[];
  namespaces: NamespaceSummaryDto[];
  languages: LanguageSummaryDto[];
  nestedArchives: NestedArchiveSummaryDto[];
  recommendedSourceLocale: string | null;
  sourceLocale: string;
  fallbackPriority: string[];
  preserveExistingJapanese: boolean;
  fillMissingFromOtherLanguages: boolean;
  includedNamespaces: IncludedNamespaceDto[];
  setupCompleted: boolean;
  existingJapaneseTranslationCount: number;
  missingSourceKeysCount: number;
  counts: ProjectCountsDto;
  targetLanguageFileName: string;
  targetLanguageFormat: "json" | "legacyLang";
  isSigned: boolean;
  assetCount: number | null;
  assetSummary?: ProjectAssetSummaryDto;
  minecraftVersionDetection?: MinecraftVersionDetectionDto | null;
  supportedMinecraftVersions?: string[];
}

export interface ReviewSetupSummaryDto {
  existingJapaneseTranslationCount: number;
  missingSourceKeysCount: number;
  assets: ProjectAssetSummaryDto;
}

export interface RecentProjectDto {
  id: string;
  displayName: string;
  modVersion: string | null;
  modCount: number;
  instancePath: string | null;
  lastOpened: string;
  translationProgress: {
    translated: number;
    missing: number;
    needsReview: number;
  };
}

export interface ReviewSettingsDto {
  sourceLocale: string;
  fallbackPriority: string[];
  preserveExistingJapanese: boolean;
  fillMissingFromOtherLanguages: boolean;
  includedNamespaces: IncludedNamespaceDto[];
}

export interface IncludedNamespaceDto {
  modUnitId: number;
  namespace: string;
}

export interface ModUnitSummaryDto {
  id: number;
  sourceKind?: "jar" | "modpack" | "shaderpack";
  sourceFilename: string;
  modId: string | null;
  name: string | null;
  version: string | null;
  loader: string | null;
  minecraftVersion: string | null;
  total: number;
  translated: number;
  needsReview: number;
  ftbQuestEntries?: number;
}

export interface ProjectEntryDto {
  id: number;
  modUnitId: number;
  namespace: string;
  key: string;
  sourceText: string | null;
  sourceLocale: string | null;
  targetText: string | null;
  targetOriginalText: string | null;
  translatedSourceText: string | null;
  origin: TranslationOrigin;
  sortIndex: number;
  flags: EntryFlag[];
}

export interface EditorEntryDto {
  id: number;
  modUnitId: number;
  entryKind?: import("./translation").EntryKind;
  namespace: string;
  key: string;
  sourceText: string | null;
  sourceLocale: string | null;
  targetText: string | null;
  targetOriginalText?: string | null;
  translatedSourceText?: string | null;
  sortIndex?: number;
  origin: TranslationOrigin;
  flags: EntryFlag[];
}
