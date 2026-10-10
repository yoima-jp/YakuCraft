import type { ModLanguageStatusDto, ModScanResultDto } from "./types";

export function isSelectedByDefault(status: ModLanguageStatusDto, thresholdPercent: number): boolean {
  switch (status.kind) {
    case "untranslated":
      return true;
    case "partial":
      return status.coverage * 100 < thresholdPercent;
    case "complete":
    case "no_language_files":
    case "failed":
      return false;
  }
}

export function defaultSelectedPaths(results: ModScanResultDto[], thresholdPercent: number): Set<string> {
  return new Set(
    results
      .filter((result) => isSelectedByDefault(result.status, thresholdPercent))
      .map((result) => result.path),
  );
}
