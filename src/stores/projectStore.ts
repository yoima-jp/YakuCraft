import { create } from "zustand";

import type {
  IncludedNamespaceDto,
  ModUnitSummaryDto,
  ProjectDetailsDto,
  RecentProjectDto,
} from "../types/project";
import type { AnalysisProgressDto, AppError, ModImportFailureDto, OperationProgressDto } from "../types/commands";

interface ProjectStore {
  sourcePath: string | null;
  sourceFileName: string | null;
  details: ProjectDetailsDto | null;
  modUnits: ModUnitSummaryDto[];
  modImportFailures: ModImportFailureDto[];
  recentProjects: RecentProjectDto[];
  analysisProgress: AnalysisProgressDto | null;
  operationProgress: OperationProgressDto | null;
  operationTitle: string;
  analysisJobId: string | null;
  analysisError: AppError | null;
  reviewError: AppError | null;
  projectOpenError: string | null;
  preserveJapanese: boolean;
  fillMissingSourceKeys: boolean;
  sourceLocale: string;
  includedNamespaces: IncludedNamespaceDto[];
  setSourceJar: (path: string, fileName: string) => void;
  setProjectDetails: (details: ProjectDetailsDto) => void;
  setModUnits: (modUnits: ModUnitSummaryDto[]) => void;
  setModImportFailures: (failures: ModImportFailureDto[]) => void;
  setRecentProjects: (projects: RecentProjectDto[]) => void;
  setAnalysisProgress: (progress: AnalysisProgressDto | null) => void;
  setOperationProgress: (progress: OperationProgressDto | null) => void;
  setOperationTitle: (title: string) => void;
  setAnalysisJobId: (jobId: string | null) => void;
  setAnalysisError: (error: AppError | null) => void;
  setReviewError: (error: AppError | null) => void;
  setProjectOpenError: (error: string | null) => void;
  setSourceLocale: (locale: string) => void;
  setPreserveJapanese: (preserve: boolean) => void;
  setFillMissingSourceKeys: (fill: boolean) => void;
  setIncludedNamespaces: (namespaces: IncludedNamespaceDto[]) => void;
}

export const useProjectStore = create<ProjectStore>()((set) => ({
  sourcePath: null,
  sourceFileName: null,
  details: null,
  modUnits: [],
  modImportFailures: [],
  recentProjects: [],
  analysisProgress: null,
  operationProgress: null,
  operationTitle: "読み込んでいます",
  analysisJobId: null,
  analysisError: null,
  reviewError: null,
  projectOpenError: null,
  preserveJapanese: true,
  fillMissingSourceKeys: true,
  sourceLocale: "en_us",
  includedNamespaces: [],
  setSourceJar: (sourcePath, sourceFileName) =>
    set({
      sourcePath,
      sourceFileName,
      details: null,
      modUnits: [],
      modImportFailures: [],
      analysisProgress: null,
      operationProgress: null,
      analysisJobId: null,
      analysisError: null,
      reviewError: null,
    }),
  setProjectDetails: (details) =>
    set({
      details,
      sourceFileName: details.mods[0]?.name ?? details.project.displayName,
      sourceLocale: details.sourceLocale,
      preserveJapanese: details.preserveExistingJapanese,
      fillMissingSourceKeys: details.fillMissingFromOtherLanguages,
      includedNamespaces: details.includedNamespaces.map((selection) => ({ ...selection })),
      reviewError: null,
    }),
  setRecentProjects: (recentProjects) => set({ recentProjects }),
  setModUnits: (modUnits) => set({ modUnits }),
  setModImportFailures: (modImportFailures) => set({ modImportFailures }),
  setAnalysisProgress: (analysisProgress) => set({ analysisProgress }),
  setOperationProgress: (operationProgress) => set({ operationProgress }),
  setOperationTitle: (operationTitle) => set({ operationTitle }),
  setAnalysisJobId: (analysisJobId) => set({ analysisJobId }),
  setAnalysisError: (analysisError) => set({ analysisError }),
  setReviewError: (reviewError) => set({ reviewError }),
  setProjectOpenError: (projectOpenError) => set({ projectOpenError }),
  setSourceLocale: (sourceLocale) => set({ sourceLocale }),
  setPreserveJapanese: (preserveJapanese) => set({ preserveJapanese }),
  setFillMissingSourceKeys: (fillMissingSourceKeys) => set({ fillMissingSourceKeys }),
  setIncludedNamespaces: (includedNamespaces) => set({ includedNamespaces }),
}));
