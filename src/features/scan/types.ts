import type { AppError } from "../../types/commands";

export type ModLanguageStatusDto =
  | { kind: "untranslated" }
  | { kind: "partial"; coverage: number }
  | { kind: "complete" }
  | { kind: "no_language_files" }
  | { kind: "failed"; reason: string };

export interface ScannedModInfoDto {
  modId: string | null;
  name: string | null;
  version: string | null;
  loader: "fabric" | "quilt" | "forge" | "neoforge" | "legacy_forge" | "unknown";
  minecraftVersion: string | null;
  minecraftVersionEstimate: string | null;
  metadataSource: string | null;
}

export interface ModScanResultDto {
  path: string;
  fileName: string;
  fileSizeBytes: number | null;
  sha256: string | null;
  mods: ScannedModInfoDto[];
  sourceKeyCount: number;
  translatedKeyCount: number;
  status: ModLanguageStatusDto;
}

export interface ShaderpackScanResultDto {
  path: string;
  fileName: string;
  fileSizeBytes: number | null;
  sha256: string | null;
  sourceKeyCount: number;
  translatedKeyCount: number;
  status: ModLanguageStatusDto;
}

export interface ModpackScanDto {
  gameDirectory: string;
  ftbQuests: "none" | "single" | "split" | "legacy_inline";
  files: Array<{
    path: string;
    kind: "ftb_quests_language" | "kube_js_language";
    locale: string;
    namespace: string | null;
  }>;
  kubejsNamespaces: string[];
  totalEntries: number;
  ftbQuestEntries: number;
  kubejsEntries: number;
  issues: string[];
}

export interface ScanProgressDto {
  completed: number;
  total: number;
  currentFile: string;
}

export type ScanModsFolderEventDto =
  | { kind: "progress"; progress: ScanProgressDto }
  | {
    kind: "completed";
    jobId: string;
    results: ModScanResultDto[];
    scannedFolder: string;
    instanceDir: string | null;
    looksLikeInstanceDir: boolean;
    modpackScan: ModpackScanDto | null;
    shaderpacks: ShaderpackScanResultDto[];
  }
  | { kind: "failed"; jobId: string; error: AppError };
