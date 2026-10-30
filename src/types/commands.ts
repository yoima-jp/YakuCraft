export interface AppError {
  code: string;
  userMessage: string;
  detail?: string;
  recoverable: boolean;
}

export type AnalysisStageDto =
  | "validateJar"
  | "sha256"
  | "scanZipPaths"
  | "parseMetadata"
  | "scanNestedJars"
  | "discoverNamespaces"
  | "parseLanguages"
  | "detectTargetTranslation";

export interface AnalysisProgressDto {
  stage: AnalysisStageDto;
  current: number;
  total: number | null;
  message: string;
  modIndex?: number;
  modCount?: number;
  modFilename?: string;
}

export interface OperationProgressDto {
  percent: number | null;
  message: string;
}

export interface ModImportFailureDto {
  filename: string;
  reason: string;
}

export type AnalysisChannelEvent =
  | { kind: "progress"; jobId: string; progress: AnalysisProgressDto; modIndex: number; modCount: number; modFilename: string }
  | { kind: "completed"; completed: { jobId: string; projectId: string; failedMods: ModImportFailureDto[] } }
  | { kind: "failed"; jobId: string; error: AppError };

export type AddUnitsChannelEventDto =
  | { kind: "progress"; jobId: string; progress: AnalysisProgressDto; modIndex: number; modCount: number; modFilename: string }
  | { kind: "completed"; jobId: string; added: number; addedUnitIds: number[]; duplicateFilenames: string[]; duplicateModIdFilenames: string[]; ignoredFilenames: string[] }
  | { kind: "failed"; jobId: string; error: AppError };
