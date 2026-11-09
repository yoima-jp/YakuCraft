import type { AppError } from "./commands";
import type { EditorEntryDto } from "./project";

export type AiProviderId =
  | "openai"
  | "openai-compatible"
  | "deepl"
  | "chatgpt"
  | "gemini"
  | "openrouter"
  | "deepseek"
  | "groq"
  | "ollama"
  | "lm-studio"
  | "anthropic"
  | "plamo";

export type PlamoModelId = "plamo-3-translate-0.8b" | "plamo-3-translate-2b" | "plamo-3-translate-8b";

export type PlamoDevice = "cuda" | "vulkan" | "cpu";

export interface AiSettingsDto {
  provider: AiProviderId;
  configured: boolean;
  accountLabel?: string;
  model: string;
  baseUrl: string;
  batchSize: number;
  concurrency: number;
  customPrompt: string;
  // PLaMo のみ。ローカル実行に使うデバイスで、未指定時はバックエンドが既定を決める。
  device?: PlamoDevice;
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
  | { kind: "batchResult"; jobId: string; entries: EditorEntryDto[]; rejected: AiRejectedEntryDto[] }
  | { kind: "completed"; jobId: string; completed: number; total: number }
  | { kind: "cancelled"; jobId: string; completed: number; total: number }
  | { kind: "failed"; jobId: string; error: AppError };

export interface PlamoModelDto {
  id: PlamoModelId;
  parameterLabel: string;
  sizeBytes: number;
  downloaded: boolean;
}

export interface PlamoStatusDto {
  models: PlamoModelDto[];
  // CUDA を提供しない環境では cuda が null。UI はこれで選択肢を出すか決める。
  runtimeReady: { cpu: boolean; vulkan: boolean; cuda: boolean | null };
  runtimeSizeBytes: { cpu: number; vulkan: number; cuda: number | null };
  devices: {
    cudaOffered: boolean;
    gpuName: string | null;
    // 未設定時に選ばれるデバイス。
    default: PlamoDevice;
  };
  licenseAccepted: boolean;
  // PLaMo が翻訳先として扱える Minecraft ロケール（小文字）。
  supportedTargetLocales: string[];
  // 画面を開き直した場合も、既に走っているダウンロードを表示できるようにする。
  downloading: boolean;
}

export type PlamoDownloadEvent =
  | { kind: "progress"; phase: "runtime" | "model"; receivedBytes: number; totalBytes: number }
  | { kind: "completed" }
  | { kind: "cancelled" }
  | { kind: "failed"; error: AppError };
