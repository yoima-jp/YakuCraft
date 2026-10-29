import type { ExportKind } from "../../types/export";
import type { ModExportResultDto } from "./exportApi";

export interface ExportRunDetails {
  kind: ExportKind;
  directInstall: boolean;
  inPlaceReplacement: boolean;
  outputPaths: string[];
  modResults: ModExportResultDto[];
  placeholderErrorCount?: number;
  conflicts: Array<{
    namespace: string;
    key: string;
    keptFrom: string;
    droppedFrom: string;
  }>;
}

let currentDetails: ExportRunDetails | null = null;

export function setExportRunDetails(details: ExportRunDetails) {
  currentDetails = details;
}

export function getExportRunDetails() {
  return currentDetails;
}
