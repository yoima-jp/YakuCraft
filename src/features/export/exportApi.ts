import { invoke } from "@tauri-apps/api/core";

import type { ExportKind, ManualPackFormatDto } from "../../types/export";
import type { MinecraftVersionDetectionDto } from "../../types/project";

export interface JarReplacementDto {
  id: number;
  modUnitId: number;
  targetPath: string;
  backupPath: string;
  previousSha256: string;
  writtenSha256: string;
  createdAt: string;
  restoredAt: string | null;
}

export interface ExportUnitStatusDto {
  id: number;
  sourceKind?: "jar" | "modpack";
  sourceFilename: string;
  name: string | null;
  version: string | null;
  sourceSha256: string;
  originalPath: string;
  originalExists: boolean;
}

export interface ExportOverviewDto {
  instancePath: string | null;
  units: ExportUnitStatusDto[];
  activeReplacements: JarReplacementDto[];
  activeFileDeployments?: FileDeploymentDto[];
}

export interface FileDeploymentDto {
  id: number;
  modUnitId: number;
  targetPath: string;
  existedBefore: boolean;
  previousSha256: string | null;
  backupPath: string | null;
  deployedSha256: string;
  deployedAt: string;
  restoredAt: string | null;
}

export interface ModExportResultDto {
  modUnitId: number;
  modName: string;
  status: "success" | "skipped" | "failed";
  outputPath: string | null;
  message: string | null;
}

export type MergedPackExportOutcomeDto =
  | { status: "overwriteRequired"; outputPath: string }
  | {
    status: "completed";
    outputPath: string;
    directlyInstalled: boolean;
    modResults: ModExportResultDto[];
    conflicts: Array<{
      namespace: string;
      key: string;
      keptFrom: string;
      droppedFrom: string;
    }>;
  };

export type RestoreJarOutcomeDto =
  | { status: "confirmationRequired"; targetPath: string }
  | { status: "restored"; targetPath: string };

export type ModpackFilesExportOutcomeDto =
  | { status: "overwriteRequired"; targetPath: string }
  | { status: "completed"; outputPath: string; directlyInstalled: boolean; fileCount: number };

export type RestoreFileDeploymentOutcomeDto = { status: "restored"; targetPath: string };

export interface SelectedExportValidationDto {
  issues: ExportIssueDto[];
  minecraftVersionDetection: MinecraftVersionDetectionDto | null;
  counts: { total: number; translated: number; missing: number; needsReview: number };
  isSigned: boolean;
}

export interface ExportIssueDto {
  severity: "warning" | "error";
  code: string;
  message: string;
  modUnitId: number | null;
  modName: string | null;
  count: number | null;
  namespace: string | null;
  key: string | null;
  displayLabel?: string | null;
  sourceText: string | null;
  targetText: string | null;
}

export class ExportCommandError extends Error {
  constructor(readonly code: string, message: string, readonly detail?: string) {
    super(message);
    this.name = "ExportCommandError";
  }
}

async function invokeExport<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error: unknown) {
    if (typeof error === "object" && error !== null) {
      const appError = error as Record<string, unknown>;
      if (typeof appError.code === "string" && typeof appError.userMessage === "string") {
        throw new ExportCommandError(
          appError.code,
          appError.userMessage,
          typeof appError.detail === "string" ? appError.detail : undefined,
        );
      }
    }
    throw new ExportCommandError("unexpected", "処理に失敗しました。もう一度お試しください。");
  }
}

export function getExportOverview(projectId: string) {
  return invokeExport<ExportOverviewDto>("get_export_overview", { projectId });
}

export function validateSelectedProject(
  projectId: string,
  kind: ExportKind,
  modUnitIds: number[],
  minecraftVersion?: string,
  namespace?: string,
  manualFormat?: ManualPackFormatDto,
) {
  return invokeExport<SelectedExportValidationDto>("validate_selected_project", {
    projectId,
    kind,
    modUnitIds,
    minecraftVersion: minecraftVersion || null,
    namespace: namespace ?? null,
    manualFormat: manualFormat ?? null,
  });
}

export function exportSelectedMergedResourcePack(
  projectId: string,
  modUnitIds: number[],
  outputPath: string | null,
  minecraftVersion: string,
  manualFormat: ManualPackFormatDto | undefined,
  directInstall: boolean,
  overwrite: boolean,
) {
  return invokeExport<MergedPackExportOutcomeDto>("export_selected_merged_resource_pack", {
    projectId,
    modUnitIds,
    outputPath,
    minecraftVersion,
    manualFormat: manualFormat ?? null,
    directInstall,
    overwrite,
  });
}

export function exportSelectedModifiedJars(
  projectId: string,
  modUnitIds: number[],
  outputDirectory: string,
  replaceInPlace: boolean,
) {
  return invokeExport<ModExportResultDto[]>("export_selected_modified_jars", {
    projectId,
    modUnitIds,
    outputDirectory,
    replaceInPlace,
  });
}

export function exportSelectedLanguageFiles(
  projectId: string,
  modUnitIds: number[],
  outputDirectory: string,
  namespace: string,
) {
  return invokeExport<ModExportResultDto[]>("export_selected_language_files", {
    projectId,
    modUnitIds,
    outputDirectory,
    namespace,
  });
}

export function exportSelectedLanguageFile(projectId: string, path: string, namespace: string) {
  return invokeExport<string>("export_language_file", { projectId, path, namespace });
}

export function restoreModifiedJar(projectId: string, replacementId: number, confirmChanged: boolean) {
  return invokeExport<RestoreJarOutcomeDto>("restore_modified_jar", {
    projectId,
    replacementId,
    confirmChanged,
  });
}

export function exportModpackFiles(
  projectId: string,
  modUnitIds: number[],
  outputPath: string | null,
  directInstall: boolean,
  overwrite: boolean,
) {
  return invokeExport<ModpackFilesExportOutcomeDto>("export_modpack_files", {
    projectId,
    modUnitIds,
    outputPath,
    directInstall,
    overwrite,
  });
}

export function restoreModpackFile(projectId: string, deploymentId: number) {
  return invokeExport<RestoreFileDeploymentOutcomeDto>("restore_modpack_file", {
    projectId,
    deploymentId,
  });
}
