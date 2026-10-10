import type { AppError } from "./commands";
import type { ProjectEntryDto } from "./project";

export type AiProviderId = "openai" | "openai-compatible" | "deepl" | "chatgpt";

export interface AiSettingsDto {
  provider: AiProviderId;
  configured: boolean;
  accountLabel?: string;
  model: string;
  baseUrl: string;
  batchSize: number;
  concurrency: number;
  customPrompt: string;
}

export interface SaveAiProviderDto extends Omit<AiSettingsDto, "configured" | "accountLabel"> {}

export interface ChatGptCredentialStatusDto {
  configured: boolean;
  provider: "chatgpt";
  accountLabel?: string;
}

export interface AiRejectedEntryDto {
  id: number;
  reasons: string[];
}

export type AiJobEventDto =
  | { kind: "progress"; jobId: string; completed: number; total: number }
  | { kind: "batchResult"; jobId: string; entries: ProjectEntryDto[]; rejected: AiRejectedEntryDto[] }
  | { kind: "completed"; jobId: string; completed: number; total: number }
  | { kind: "cancelled"; jobId: string; completed: number; total: number }
  | { kind: "failed"; jobId: string; error: AppError };
