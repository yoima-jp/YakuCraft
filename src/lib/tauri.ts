import { Channel, invoke } from "@tauri-apps/api/core";

import { logFrontendError } from "./errorLogging";
import type { AnalysisChannelEvent, AppError, OperationProgressDto } from "../types/commands";
export type { AnalysisChannelEvent, AnalysisProgressDto, AppError } from "../types/commands";
import type { AiJobEventDto, AiProviderId, AiSettingsDto, ChatGptCredentialStatusDto, SaveAiProviderDto } from "../types/ai";
import type { ExportKind, ExportValidationDto, ManualPackFormatDto } from "../types/export";
import type {
  ProjectDetailsDto,
  EditorEntryDto,
  ReviewSetupSummaryDto,
  RecentProjectDto,
  ReviewSettingsDto,
  ModUnitSummaryDto,
  ModUnitRemovalInfoDto,
  ProjectSummaryDto,
} from "../types/project";
import type { GlossaryCandidate, GlossaryTerm, GlossaryTermInput, TranslationEntry } from "../types/translation";
import type { AppInfoDto, AppSettingsDto } from "../types/settings";
import type {
  MinecraftJarCandidate,
  PreviewManifest,
  PreviewOptions,
} from "../types/preview";

export class AppCommandError extends Error {
  constructor(readonly appError: AppError) {
    super(appError.userMessage);
    this.name = "AppCommandError";
  }
}

function isAppError(value: unknown): value is AppError {
  if (typeof value !== "object" || value === null) {
    return false;
  }

  const error = value as Record<string, unknown>;
  return (
    typeof error.code === "string" &&
    typeof error.userMessage === "string" &&
    typeof error.recoverable === "boolean" &&
    (error.detail === undefined || typeof error.detail === "string")
  );
}

export async function invokeApp<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error: unknown) {
    void logFrontendError(error, `Tauri command: ${command}`);
    if (isAppError(error)) {
      throw new AppCommandError(error);
    }

    throw new AppCommandError({
      code: "unexpected",
      userMessage: "処理に失敗しました。もう一度お試しください。",
      recoverable: true,
    });
  }
}

export function exitApp() {
  return invokeApp<void>("exit_app");
}

export function getSettings() {
  return invokeApp<AppSettingsDto>("get_settings");
}

export function saveSettings(settings: Record<string, unknown>) {
  return invokeApp<void>("save_settings", { settings });
}

export function getAppInfo() {
  return invokeApp<AppInfoDto>("get_app_info");
}

export function automaticUpdatesSupported() {
  return invokeApp<boolean>("automatic_updates_supported");
}

export function clearCache() {
  return invokeApp<void>("clear_cache");
}

export function getRecentLog() {
  return invokeApp<string>("get_recent_log");
}

export function openLogFolder() {
  return invokeApp<void>("open_log_folder");
}

export function getAiSettings(provider?: AiProviderId) {
  return invokeApp<AiSettingsDto>("get_ai_settings", { provider });
}

export function saveAiProvider(config: SaveAiProviderDto, apiKey?: string) {
  return invokeApp<AiSettingsDto>("save_ai_provider", { config, apiKey });
}

export function deleteAiProvider(provider: AiProviderId) {
  return invokeApp<void>("delete_ai_provider", { provider });
}

export function testAiProvider(provider: AiProviderId) {
  return invokeApp<void>("test_ai_provider", { provider });
}

export function listAiModels(provider: AiProviderId, baseUrl: string, apiKey?: string) {
  return invokeApp<string[]>("list_ai_models", { provider, baseUrl, apiKey });
}

export function startChatgptLogin() {
  return invokeApp<ChatGptCredentialStatusDto>("start_chatgpt_login");
}

export function cancelChatgptLogin() {
  return invokeApp<boolean>("cancel_chatgpt_login");
}

export function logoutChatgpt() {
  return invokeApp<void>("logout_chatgpt");
}

export function startAiJob(
  entryIds: number[],
  provider: AiProviderId,
  expectedEntries: Array<Pick<TranslationEntry, "id" | "targetText" | "origin">>,
  modNamesByUnitAndNamespace: Record<number, Record<string, string>>,
  expectedProjectId: string,
  onEvent: (event: AiJobEventDto) => void,
) {
  const channel = new Channel<AiJobEventDto>();
  channel.onmessage = onEvent;
  return invokeApp<string>("start_ai_job", {
    entryIds,
    provider,
    expectedEntries,
    modNamesByUnitAndNamespace,
    expectedProjectId,
    channel,
  });
}

export function cancelAiJob(jobId: string) {
  return invokeApp<boolean>("cancel_ai_job", { jobId });
}

export function createProjectFromJars(
  paths: string[],
  instancePath: string | null,
  onEvent: (event: AnalysisChannelEvent) => void,
  includeModpack = false,
) {
  const channel = new Channel<AnalysisChannelEvent>();
  channel.onmessage = onEvent;
  return invokeApp<string>("create_project_from_jars", { paths, instancePath, includeModpack, channel });
}

export function cancelAnalysis(jobId: string) {
  return invokeApp<boolean>("cancel_analysis", { jobId });
}

export function getProjectSummary() {
  return invokeApp<ProjectDetailsDto>("get_project_summary");
}

export function listModUnits() {
  return invokeApp<ModUnitSummaryDto[]>("list_mod_units");
}

export function getReviewAssetSummary(
  settings: ReviewSettingsDto,
  onProgress: (progress: OperationProgressDto) => void,
) {
  const channel = new Channel<OperationProgressDto>();
  channel.onmessage = onProgress;
  return invokeApp<ReviewSetupSummaryDto>("get_review_asset_summary", { settings, channel });
}

export function saveReviewSettings(
  settings: ReviewSettingsDto,
  onProgress: (progress: OperationProgressDto) => void,
) {
  const channel = new Channel<OperationProgressDto>();
  channel.onmessage = onProgress;
  return invokeApp<ProjectDetailsDto>("save_review_settings", { settings, channel });
}

export function openProject(
  projectId: string,
  onProgress: (progress: OperationProgressDto) => void,
) {
  const channel = new Channel<OperationProgressDto>();
  channel.onmessage = onProgress;
  return invokeApp<ProjectDetailsDto>("open_project", { projectId, channel });
}

export function listRecentProjects() {
  return invokeApp<RecentProjectDto[]>("list_recent_projects");
}

export function renameProject(projectId: string, name: string) {
  return invokeApp<ProjectSummaryDto>("rename_project", { projectId, name });
}

export function getModUnitRemovalInfo(modUnitId: number) {
  return invokeApp<ModUnitRemovalInfoDto>("get_mod_unit_removal_info", { modUnitId });
}

export function removeModUnit(modUnitId: number, confirmUnrestoredReplacements: boolean) {
  return invokeApp<ProjectSummaryDto>("remove_mod_unit", { modUnitId, confirmUnrestoredReplacements });
}

export function refreshModpackUnit(modUnitId: number) {
  return invokeApp<void>("refresh_modpack_unit", { modUnitId });
}

export function deleteProject(projectId: string) {
  return invokeApp<void>("delete_project", { projectId });
}

export async function listEntries(modUnitId: number | null = null): Promise<TranslationEntry[]> {
  const entries = await invokeApp<EditorEntryDto[]>("list_entries", { modUnitId });
  return entries.map(entryFromProject);
}

export function listGlossaryTerms() {
  return invokeApp<GlossaryTerm[]>("list_glossary_terms");
}

export function upsertGlossaryTerm(term: GlossaryTermInput) {
  return invokeApp<GlossaryTerm[]>("upsert_glossary_term", { term, id: null });
}

export function updateGlossaryTerm(id: number, term: GlossaryTermInput) {
  return invokeApp<GlossaryTerm[]>("upsert_glossary_term", { id, term });
}

export function upsertGlossaryTerms(terms: GlossaryTermInput[]) {
  return invokeApp<GlossaryTerm[]>("upsert_glossary_terms", { terms });
}

export function deleteGlossaryTerm(id: number) {
  return invokeApp<GlossaryTerm[]>("delete_glossary_term", { id });
}

export function importGlossary(path: string) {
  return invokeApp<GlossaryTerm[]>("import_glossary", { path });
}

export function exportGlossary(path: string) {
  return invokeApp<number>("export_glossary", { path });
}

export function glossaryCandidates() {
  return invokeApp<GlossaryCandidate[]>("glossary_candidates");
}

export async function updateEntriesBatch(
  updates: Array<Pick<TranslationEntry, "id" | "targetText">>,
): Promise<TranslationEntry[]> {
  const entries = await invokeApp<EditorEntryDto[]>("update_entries_batch", {
    updates: updates.map((entry) => ({
      id: entry.id,
      targetText: entry.targetText,
      origin: "manual",
    })),
  });
  return entries.map(entryFromProject);
}

export function entryFromProject(entry: EditorEntryDto): TranslationEntry {
  return {
    id: entry.id,
    modUnitId: entry.modUnitId,
    entryKind: entry.entryKind,
    namespace: entry.namespace,
    key: entry.key,
    sourceText: entry.sourceText,
    sourceLocale: entry.sourceLocale,
    targetText: entry.targetText ?? "",
    origin: entry.origin,
    flags: entry.flags,
  };
}

export function validateProject(
  kind: ExportKind,
  minecraftVersion?: string,
  namespace?: string,
  manualFormat?: ManualPackFormatDto,
) {
  return invokeApp<ExportValidationDto>("validate_project", {
    kind,
    minecraftVersion: minecraftVersion || null,
    namespace: namespace ?? null,
    manualFormat: manualFormat ?? null,
  });
}

export function exportLanguageFile(path: string, namespace: string) {
  return invokeApp<string>("export_language_file", { path, namespace });
}

export function exportResourcePack(
  path: string,
  minecraftVersion: string,
  manualFormat?: ManualPackFormatDto,
) {
  return invokeApp<string>("export_resource_pack", {
    path,
    minecraftVersion,
    manualFormat: manualFormat ?? null,
  });
}

export function exportModifiedJar(path: string) {
  return invokeApp<string>("export_modified_jar", { path });
}

export function openOutputFolder(path: string) {
  return invokeApp<void>("open_output_folder", { path });
}

export function getAssetPreviewManifest(entryId: number, variant?: PreviewOptions) {
  return invokeApp<PreviewManifest>("get_asset_preview_manifest", {
    entryId,
    variant: variant ?? null,
  });
}

export function loadThumbnail(projectId: string, key: string) {
  return invokeApp<number[] | null>("load_thumbnail", { projectId, key });
}

export function saveThumbnail(projectId: string, key: string, bytes: number[]) {
  return invokeApp<void>("save_thumbnail", { projectId, key, bytes });
}

export async function getAssetBytes(source: string, path: string): Promise<Uint8Array> {
  const bytes = await invoke<ArrayBuffer | Uint8Array>("get_asset_bytes", { source, path });
  return bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
}

export function detectMinecraftJars() {
  return invokeApp<MinecraftJarCandidate[]>("detect_minecraft_jars");
}

export function getMinecraftJarSetting() {
  return invokeApp<string | null>("get_minecraft_jar_setting");
}

export function setMinecraftJar(path: string | null) {
  return invokeApp<void>("set_minecraft_jar", { path });
}

export function isShaderpackFolder(path: string) {
  return invokeApp<boolean>("is_shaderpack_folder", { path });
}
