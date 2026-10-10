import type { MinecraftVersionDetectionDto } from "./project";

export type ExportKind = "languageFile" | "resourcePack" | "modifiedJar";

export interface ExportIssueDto {
  severity: "warning" | "error";
  code: string;
  message: string;
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
