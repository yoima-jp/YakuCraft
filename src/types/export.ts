import type { MinecraftVersionDetectionDto } from "./project";

export type ExportKind = "languageFile" | "resourcePack" | "modifiedJar" | "modpackFiles" | "shaderpackZip";

export interface ExportIssueDto {
  severity: "warning" | "error";
  code: string;
  message: string;
  displayLabel?: string | null;
}

export interface ExportValidationDto {
  issues: ExportIssueDto[];
  minecraftVersionDetection: MinecraftVersionDetectionDto | null;
  counts: {
    total: number;
    translated: number;
    missing: number;
    needsReview: number;
  };
  isSigned: boolean;
}

export interface ManualPackFormatDto {
  major: number;
  minor: number;
}
