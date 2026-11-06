import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import { PrimaryButton, SecondaryButton, TertiaryButton } from "../../components/button/Buttons";
import { Checkbox, NumberInput } from "../../components/Controls";
import { ProgressBar } from "../../components/progress/ProgressBar";
import {
  AppCommandError,
  cancelAnalysis,
  createProjectFromJars,
  getProjectSummary,
  listModUnits,
} from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import type { AnalysisChannelEvent, AppError } from "../../types/commands";
import { formatNumber, lowerText, t, useT, useUiLanguage } from "../../i18n";
import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import { isAnalysisEventForJob } from "./analysisEvents";
import { defaultSelectedPaths } from "./defaultSelection";
import { useScanStore } from "./scanStore";
import { scanModsFolder } from "./tauri";
import type { ModLanguageStatusDto, ModScanResultDto, ModpackScanDto, ScanModsFolderEventDto, ShaderpackScanResultDto } from "./types";
import "./ScanScreen.css";

const FILTER_KINDS = ["untranslated", "partial", "complete", "no_language_files", "failed"] as const satisfies readonly ModLanguageStatusDto["kind"][];

type StatusFilters = Record<ModLanguageStatusDto["kind"], boolean>;

const DEFAULT_STATUS_FILTERS: StatusFilters = {
  untranslated: true,
  partial: true,
  complete: true,
  no_language_files: false,
  failed: false,
};

function appError(error: unknown): AppError {
  void logFrontendError(error, "scan screen");
  return error instanceof AppCommandError
    ? error.appError
    : { code: "unexpected", userMessage: t("common.operationFailed"), recoverable: true };
}

function statusLabel(status: ModLanguageStatusDto, labels: Record<ModLanguageStatusDto["kind"], string>): string {
  if (status.kind === "partial") return t("scan.partialCoverage", { coverage: Math.round(status.coverage * 100) });
  return labels[status.kind];
}

function modName(result: ModScanResultDto): string {
  return result.mods[0]?.name ?? result.mods[0]?.modId ?? result.fileName;
}

function isSelectable(result: ModScanResultDto): boolean {
  return result.status.kind !== "no_language_files" && result.status.kind !== "failed";
}

function modpackHasImportableFiles(scan: ModpackScanDto): boolean {
  return scan.files.some((file) => file.locale.toLowerCase() === "en_us");
}

function shaderpackSelectable(result: ShaderpackScanResultDto): boolean {
  return result.status.kind !== "no_language_files" && result.status.kind !== "failed";
}

export function ScanScreen() {
  const translate = useT();
  const uiLanguage = useUiLanguage();
  const statusLabels = {
    untranslated: translate("scan.statusUntranslated"),
    partial: translate("scan.statusPartial"),
    complete: translate("scan.statusComplete"),
    no_language_files: translate("scan.statusNoLanguages"),
    failed: translate("scan.statusFailed"),
  };
  const folderPath = useScanStore((state) => state.folderPath);
  const scanRevision = useScanStore((state) => state.scanRevision);
  const scanJobId = useScanStore((state) => state.scanJobId);
  const scanProgress = useScanStore((state) => state.scanProgress);
  const results = useScanStore((state) => state.results);
  const shaderpacks = useScanStore((state) => state.shaderpacks);
  const modpackScan = useScanStore((state) => state.modpackScan);
  const scannedFolder = useScanStore((state) => state.scannedFolder);
  const instanceDir = useScanStore((state) => state.instanceDir);
  const looksLikeInstanceDir = useScanStore((state) => state.looksLikeInstanceDir);
  const scanError = useScanStore((state) => state.scanError);
  const setScanJobId = useScanStore((state) => state.setScanJobId);
  const setScanProgress = useScanStore((state) => state.setScanProgress);
  const setScanResults = useScanStore((state) => state.setScanResults);
  const setScanError = useScanStore((state) => state.setScanError);
  const setCreationComplete = useScanStore((state) => state.setCreationComplete);
  const setAnalysisProgress = useProjectStore((state) => state.setAnalysisProgress);
  const setAnalysisJobId = useProjectStore((state) => state.setAnalysisJobId);
  const setAnalysisError = useProjectStore((state) => state.setAnalysisError);
  const setSourceJar = useProjectStore((state) => state.setSourceJar);
  const setModImportFailures = useProjectStore((state) => state.setModImportFailures);
  const setProjectDetails = useProjectStore((state) => state.setProjectDetails);
  const setModUnits = useProjectStore((state) => state.setModUnits);
  const goTo = useAppStore((state) => state.goTo);

  const [thresholdInput, setThresholdInput] = useState("100");
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(() => new Set());
  const [selectedModpack, setSelectedModpack] = useState(false);
  const [statusFilters, setStatusFilters] = useState<StatusFilters>(DEFAULT_STATUS_FILTERS);
  const [search, setSearch] = useState("");
  const [scanCancelRequested, setScanCancelRequested] = useState(false);
  const [creating, setCreating] = useState(false);
  const scanJobRef = useRef<string | null>(null);
  const scanGenerationRef = useRef(0);
  const terminalScanJobsRef = useRef(new Set<string>());
  const terminalCreateJobsRef = useRef(new Set<string>());
  const createGenerationRef = useRef(0);
  const listRef = useRef<HTMLUListElement>(null);
  const selectedCountText = (count: number) => count === 1
    ? translate("scan.selectedCount_one", { count: formatNumber(count) })
    : translate("scan.selectedCount", { count: formatNumber(count) });
  const modCountText = (count: number) => count === 1
    ? translate("scan.modCount_one", { count: formatNumber(count) })
    : translate("scan.modCount", { count: formatNumber(count) });
  const shaderpackCountText = (count: number) => count === 1
    ? translate("scan.shaderpackCount_one", { count: formatNumber(count) })
    : translate("scan.shaderpackCount", { count: formatNumber(count) });
  const entryCountText = (count: number) => count === 1
    ? translate("scan.entryCount_one", { count: formatNumber(count) })
    : translate("scan.entryCount", { count: formatNumber(count) });
  const createProjectText = (count: number) => count === 1
    ? translate("scan.createProject_one", { count: formatNumber(count) })
    : translate("scan.createProject", { count: formatNumber(count) });

  const thresholdPercent = thresholdInput === ""
    ? 100
    : Math.max(0, Math.min(100, Number.parseInt(thresholdInput, 10) || 0));

  useEffect(() => {
    if (!folderPath) return;

    const generation = ++scanGenerationRef.current;
    let active = true;
    scanJobRef.current = null;
    setScanCancelRequested(false);

    const onEvent = (event: ScanModsFolderEventDto) => {
      if (!active || generation !== scanGenerationRef.current) return;
      if (event.kind === "progress") {
        setScanProgress(event.progress);
        return;
      }

      terminalScanJobsRef.current.add(event.jobId);
      scanJobRef.current = null;
      setScanJobId(null);
      if (event.kind === "completed") {
        setScanResults(event.results, event.scannedFolder, event.instanceDir, event.looksLikeInstanceDir, event.modpackScan, event.shaderpacks);
      } else {
        setScanError(event.error);
      }
    };

    void scanModsFolder(folderPath, onEvent)
      .then((jobId) => {
        if (!active || generation !== scanGenerationRef.current) {
          if (!terminalScanJobsRef.current.has(jobId)) void cancelAnalysis(jobId).catch(() => {});
          return;
        }
        if (!terminalScanJobsRef.current.has(jobId)) {
          scanJobRef.current = jobId;
          setScanJobId(jobId);
        }
      })
      .catch((error: unknown) => {
        if (active && generation === scanGenerationRef.current) setScanError(appError(error));
      });

    return () => {
      active = false;
      const jobId = scanJobRef.current;
      scanJobRef.current = null;
      if (jobId) void cancelAnalysis(jobId).catch(() => {});
    };
  }, [folderPath, scanRevision, setScanError, setScanJobId, setScanProgress, setScanResults]);

  useEffect(() => {
    if (!results) return;
    const selected = defaultSelectedPaths(results, thresholdPercent);
    for (const shaderpack of shaderpacks) {
      if (shaderpackSelectable(shaderpack)
        && (shaderpack.status.kind === "untranslated"
          || (shaderpack.status.kind === "partial" && shaderpack.status.coverage * 100 < thresholdPercent))) {
        selected.add(shaderpack.path);
      }
    }
    setSelectedPaths(selected);
  }, [results, shaderpacks, thresholdPercent]);

  useEffect(() => {
    setSelectedModpack(Boolean(modpackScan && modpackHasImportableFiles(modpackScan)));
  }, [results, modpackScan]);

  const visibleResults = useMemo(() => {
    if (!results) return [];
    const query = lowerText(search.trim(), uiLanguage);
    return results.filter((result) => {
      if (!statusFilters[result.status.kind]) return false;
      if (!query) return true;
      return [modName(result), result.fileName, result.mods[0]?.modId ?? ""]
        .some((value) => lowerText(value, uiLanguage).includes(query));
    });
  }, [results, search, statusFilters, uiLanguage]);

  const visibleShaderpacks = useMemo(() => {
    const query = lowerText(search.trim(), uiLanguage);
    return shaderpacks.filter((result) => statusFilters[result.status.kind]
      && (!query || lowerText(result.fileName, uiLanguage).includes(query)));
  }, [search, shaderpacks, statusFilters, uiLanguage]);

  const showModpack = Boolean(modpackScan);
  const modpackSelectable = Boolean(modpackScan && modpackHasImportableFiles(modpackScan));
  const showShaderpacks = visibleShaderpacks.length > 0;

  // TanStack Virtual はスクロール位置に合わせて可視行を差し替えるため、戻り値をReact Compilerでmemo化しない。
  // oxlint-disable-next-line react/incompatible-library
  const virtualizer = useVirtualizer({
    count: visibleResults.length + visibleShaderpacks.length + Number(showModpack),
    getScrollElement: () => listRef.current,
    estimateSize: (index) => (showModpack && index === 0 ? 104 : 88),
    getItemKey: (index) => {
      if (showModpack && index === 0) return "modpack";
      const itemIndex = index - Number(showModpack);
      return visibleResults[itemIndex]?.path
        ?? visibleShaderpacks[itemIndex - visibleResults.length]?.path
        ?? index;
    },
    overscan: 8,
  });

  const visibleSelectable = visibleResults.filter(isSelectable);
  const visibleSelectableShaderpacks = visibleShaderpacks.filter(shaderpackSelectable);
  const allVisibleSelected = (visibleSelectable.length > 0 || visibleSelectableShaderpacks.length > 0 || (showModpack && modpackSelectable))
    && visibleSelectable.every((result) => selectedPaths.has(result.path))
    && visibleSelectableShaderpacks.every((result) => selectedPaths.has(result.path))
    && (!showModpack || !modpackSelectable || selectedModpack);

  const updateVisibleSelection = useCallback((selected: boolean) => {
    setSelectedPaths((current) => {
      const next = new Set(current);
      for (const result of visibleSelectable) {
        if (selected) next.add(result.path);
        else next.delete(result.path);
      }
      for (const result of visibleSelectableShaderpacks) {
        if (selected) next.add(result.path);
        else next.delete(result.path);
      }
      return next;
    });
    if (showModpack && modpackSelectable) setSelectedModpack(selected);
  }, [modpackSelectable, showModpack, visibleSelectable, visibleSelectableShaderpacks]);

  const handleAnalysisEvent = useCallback((
    event: AnalysisChannelEvent,
    generation: number,
    jobId: string,
    sourcePath: string,
    sourceLabel: string,
  ) => {
    if (
      generation !== createGenerationRef.current
      || !isAnalysisEventForJob(event, jobId, useProjectStore.getState().analysisJobId)
    ) return;
    if (event.kind === "progress") {
      setAnalysisProgress({
        ...event.progress,
        modIndex: event.modIndex,
        modCount: event.modCount,
        modFilename: event.modFilename,
      });
      return;
    }

    terminalCreateJobsRef.current.add(event.kind === "completed" ? event.completed.jobId : event.jobId);
    setAnalysisJobId(null);
    if (event.kind === "failed") {
      setAnalysisError(event.error);
      setCreating(false);
      return;
    }

    setModImportFailures(event.completed.failedMods);
    void Promise.all([getProjectSummary(), listModUnits()])
      .then(async ([details, units]) => {
        const currentState = useProjectStore.getState();
        if (
          generation !== createGenerationRef.current
          || currentState.sourcePath !== sourcePath
          || currentState.sourceFileName !== sourceLabel
          || (currentState.analysisJobId !== null && currentState.analysisJobId !== jobId)
        ) return;
        setProjectDetails(details);
        setModUnits(units);
        setAnalysisProgress(null);
        setAnalysisError(null);
        setCreating(false);
        setCreationComplete(true);
        if (event.completed.failedMods.length === 0 && useAppStore.getState().route === "analyze") {
          await goTo("review");
        }
      })
      .catch((error: unknown) => {
        const currentState = useProjectStore.getState();
        if (
          generation !== createGenerationRef.current
          || currentState.sourcePath !== sourcePath
          || currentState.sourceFileName !== sourceLabel
          || (currentState.analysisJobId !== null && currentState.analysisJobId !== jobId)
        ) return;
        setAnalysisError(appError(error));
        setCreating(false);
      });
  }, [goTo, setAnalysisError, setAnalysisJobId, setAnalysisProgress, setCreationComplete, setModImportFailures, setModUnits, setProjectDetails]);

  const createSelectedProject = async () => {
    if (!results || (selectedPaths.size === 0 && !selectedModpack) || creating) return;
    const selectedMods = results.filter((result) => selectedPaths.has(result.path)).map((result) => result.path);
    const selectedShaderpacks = shaderpacks.filter((result) => selectedPaths.has(result.path)).map((result) => result.path);
    const paths = [...selectedMods, ...selectedShaderpacks];
    if (paths.length === 0 && !selectedModpack) return;

    const generation = ++createGenerationRef.current;
    setCreating(true);
    setCreationComplete(false);
    setAnalysisError(null);
    setAnalysisProgress(null);
    setAnalysisJobId(null);
    setModImportFailures([]);
    const sourceLabel = [
      selectedMods.length > 0 ? modCountText(selectedMods.length) : "",
      selectedShaderpacks.length > 0 ? shaderpackCountText(selectedShaderpacks.length) : "",
      selectedModpack ? translate("scan.modpackUnique") : "",
    ].filter(Boolean).join(translate("scan.selectionSeparator"));
    const sourcePath = paths[0] ?? instanceDir ?? scannedFolder ?? folderPath ?? "";
    setSourceJar(sourcePath, sourceLabel);
    await goTo("analyze");
    try {
      let jobId: string | null = null;
      const earlyEvents: AnalysisChannelEvent[] = [];
      // Channel通知がinvokeの返すjobIdより先に届いても、ID確定後に照合してから反映する。
      const returnedJobId = await createProjectFromJars(
        paths,
        selectedModpack || scannedFolder !== folderPath || looksLikeInstanceDir ? instanceDir : null,
        (event) => {
          if (jobId === null) earlyEvents.push(event);
          else handleAnalysisEvent(event, generation, jobId, sourcePath, sourceLabel);
        },
        selectedModpack,
      );
      jobId = returnedJobId;
      const currentState = useProjectStore.getState();
      if (
        generation !== createGenerationRef.current
        || currentState.sourcePath !== sourcePath
        || currentState.sourceFileName !== sourceLabel
        || (currentState.analysisJobId !== null && currentState.analysisJobId !== returnedJobId)
      ) {
        if (!terminalCreateJobsRef.current.has(returnedJobId)) void cancelAnalysis(returnedJobId).catch(() => {});
        return;
      }
      if (!terminalCreateJobsRef.current.has(returnedJobId)) setAnalysisJobId(returnedJobId);
      for (const event of earlyEvents) {
        handleAnalysisEvent(event, generation, returnedJobId, sourcePath, sourceLabel);
      }
    } catch (error: unknown) {
      if (
        generation !== createGenerationRef.current
        || useProjectStore.getState().sourcePath !== sourcePath
        || useProjectStore.getState().sourceFileName !== sourceLabel
      ) return;
      setAnalysisError(appError(error));
      setCreating(false);
    }
  };

  const cancelFolderScan = async () => {
    if (!scanJobId) return;
    setScanCancelRequested(true);
    try {
      await cancelAnalysis(scanJobId);
    } catch (error: unknown) {
      setScanCancelRequested(false);
      setScanError(appError(error));
    }
  };

  if (!results) {
    const progressPercent = scanProgress && scanProgress.total > 0
      ? Math.round((scanProgress.completed / scanProgress.total) * 100)
      : 0;
    return (
      <section className="focus-page analyze-screen scan-screen" aria-labelledby="scan-title">
        <div className="analyze-screen__content">
          <h1 className="page-title" id="scan-title">{translate("scan.scanningFolder")}</h1>
          {folderPath && <p className="scan-screen__path selectable">{folderPath}</p>}
          {scanError ? (
            <div className="scan-screen__error" role="alert">
              <p>{scanError.userMessage}</p>
              {scanError.detail && <pre className="selectable">{scanError.detail}</pre>}
              <SecondaryButton onClick={() => useScanStore.getState().retryScan()}>{translate("scan.retry")}</SecondaryButton>
            </div>
          ) : (
            <>
              <ProgressBar value={progressPercent} label={translate("scan.progress")} />
              <p className="analyze-screen__stage" aria-live="polite">
                {scanProgress
                  ? `${formatNumber(scanProgress.completed)} / ${formatNumber(scanProgress.total)} · ${scanProgress.currentFile}`
                  : translate("common.preparing")}
              </p>
              {scanJobId && (
                <SecondaryButton disabled={scanCancelRequested} onClick={() => void cancelFolderScan()}>
                  {scanCancelRequested ? translate("scan.canceling") : translate("common.cancel")}
                </SecondaryButton>
              )}
            </>
          )}
        </div>
      </section>
    );
  }

  return (
    <section className="focus-page scan-screen" aria-labelledby="scan-results-title">
      <header className="scan-screen__header">
        <div>
          <h1 className="page-title" id="scan-results-title">{translate("scan.resultsTitle")}</h1>
          <p className="scan-screen__path selectable">{scannedFolder ?? folderPath}</p>
        </div>
        {looksLikeInstanceDir ? null : (
          <p className="scan-screen__instance-note">
            {translate("scan.instanceUnavailable")}
          </p>
        )}
      </header>

      <div className="scan-screen__controls">
        <label className="scan-screen__search">
          <span>{translate("scan.searchByName")}</span>
          <input
            type="search"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            value={search}
            onChange={(event) => setSearch(event.currentTarget.value)}
          />
        </label>
        <div className="scan-screen__threshold">
          <span>{translate("scan.partialThreshold")}</span>
          <span className="scan-screen__threshold-input">
            <NumberInput
              aria-label={translate("scan.partialThresholdInput")}
              min={0}
              max={100}
              value={thresholdInput}
              onChange={(event) => setThresholdInput(event.currentTarget.value)}
            />
            <span>{translate("scan.lessThan")}</span>
          </span>
        </div>
      </div>

      <fieldset className="scan-screen__filters">
        <legend>{translate("scan.statusFilter")}</legend>
        {FILTER_KINDS.map((kind) => (
          <Checkbox
            checked={statusFilters[kind]}
            key={kind}
            onChange={(checked) => setStatusFilters((current) => ({ ...current, [kind]: checked }))}
          >
            {statusLabels[kind]}
          </Checkbox>
        ))}
      </fieldset>

      <div className="scan-screen__selection-bar">
        <output aria-live="polite">{selectedCountText(selectedPaths.size + Number(selectedModpack))}</output>
        <div>
          <TertiaryButton disabled={(visibleSelectable.length === 0 && visibleSelectableShaderpacks.length === 0 && !(showModpack && modpackSelectable)) || allVisibleSelected} onClick={() => updateVisibleSelection(true)}>
            {translate("scan.selectVisible")}
          </TertiaryButton>
          <TertiaryButton disabled={(visibleSelectable.length === 0 && visibleSelectableShaderpacks.length === 0 && !(showModpack && modpackSelectable)) || (selectedPaths.size === 0 && !selectedModpack)} onClick={() => updateVisibleSelection(false)}>
            {translate("scan.clearVisible")}
          </TertiaryButton>
        </div>
      </div>

      {visibleResults.length > 0 || showModpack || showShaderpacks ? (
        <ul
          className="scan-screen__list"
          ref={listRef}
          aria-label={translate("scan.resultsList")}
        >
          <li className="scan-screen__list-space" aria-hidden="true" style={{ height: virtualizer.getTotalSize() }} />
          {virtualizer.getVirtualItems().map((virtualRow) => {
            const isModpackRow = showModpack && virtualRow.index === 0;
            if (isModpackRow && modpackScan) {
              const oldFtb = modpackScan.ftbQuests === "legacy_inline";
              return (
                <li
                  className="scan-screen__row"
                  key="modpack"
                  data-selected={selectedModpack || undefined}
                  style={{ transform: `translateY(${virtualRow.start}px)`, height: virtualRow.size }}
                >
                  <Checkbox
                    className="scan-screen__row-checkbox"
                    checked={selectedModpack}
                    disabled={!modpackSelectable}
                    onChange={setSelectedModpack}
                  >
                    <span className="scan-screen__row-content">
                      <span className="scan-screen__row-heading">
                        <strong>{translate("scan.modpackName")}</strong>
                        <span>{entryCountText(modpackScan.totalEntries)}</span>
                        <span className="scan-screen__status" data-status={oldFtb ? "partial" : "untranslated"}>
                          {oldFtb ? translate("scan.legacyQuestFormat") : translate("scan.translatable")}
                        </span>
                      </span>
                      <span className="scan-screen__filename">
                        {translate("scan.modpackContents", { quests: formatNumber(modpackScan.ftbQuestEntries), kubejs: formatNumber(modpackScan.kubejsEntries) })}
                      </span>
                      {oldFtb && (
                        <span className="scan-screen__reason">
                          {modpackScan.kubejsNamespaces.length > 0
                            ? translate("scan.legacyQuestUnsupportedWithKubeJs")
                            : translate("scan.legacyQuestUnsupported")}
                        </span>
                      )}
                    </span>
                  </Checkbox>
                </li>
              );
            }
            const resultIndex = virtualRow.index - Number(showModpack);
            const result = visibleResults[resultIndex];
            if (!result) {
              const shaderpack = visibleShaderpacks[resultIndex - visibleResults.length];
              if (!shaderpack) return null;
              const selectable = shaderpackSelectable(shaderpack);
              const checked = selectedPaths.has(shaderpack.path);
              return (
                <li
                  className="scan-screen__row"
                  key={shaderpack.path}
                  data-selected={checked || undefined}
                  style={{ transform: `translateY(${virtualRow.start}px)`, height: virtualRow.size }}
                >
                  <Checkbox
                    className="scan-screen__row-checkbox"
                    checked={checked}
                    disabled={!selectable}
                    onChange={(nextChecked) => setSelectedPaths((current) => {
                      const next = new Set(current);
                      if (nextChecked) next.add(shaderpack.path);
                      else next.delete(shaderpack.path);
                      return next;
                    })}
                  >
                    <span className="scan-screen__row-content">
                      <span className="scan-screen__row-heading">
                        <strong>{translate("scan.shaderpackName")}</strong>
                        <span>{entryCountText(shaderpack.sourceKeyCount)}</span>
                        <span className="scan-screen__status" data-status={shaderpack.status.kind}>{statusLabel(shaderpack.status, statusLabels)}</span>
                      </span>
                      <span className="scan-screen__filename">{shaderpack.fileName}</span>
                      {shaderpack.status.kind === "failed" && <span className="scan-screen__reason selectable">{shaderpack.status.reason}</span>}
                    </span>
                  </Checkbox>
                </li>
              );
            }
            const selectable = isSelectable(result);
            const label = modName(result);
            const version = result.mods[0]?.version ?? translate("scan.unknownVersion");
            const checked = selectedPaths.has(result.path);
            return (
              <li
                className="scan-screen__row"
                key={result.path}
                data-selected={checked || undefined}
                style={{ transform: `translateY(${virtualRow.start}px)`, height: virtualRow.size }}
              >
                <Checkbox
                  className="scan-screen__row-checkbox"
                  checked={checked}
                  disabled={!selectable}
                  onChange={(nextChecked) => setSelectedPaths((current) => {
                    const next = new Set(current);
                    if (nextChecked) next.add(result.path);
                    else next.delete(result.path);
                    return next;
                  })}
                >
                  <span className="scan-screen__row-content">
                    <span className="scan-screen__row-heading">
                      <strong>{label}</strong>
                      <span>{version}</span>
                      <span className="scan-screen__status" data-status={result.status.kind}>{statusLabel(result.status, statusLabels)}</span>
                    </span>
                    <span className="scan-screen__filename">{result.fileName}</span>
                    {result.status.kind === "failed" && <span className="scan-screen__reason selectable">{result.status.reason}</span>}
                  </span>
                </Checkbox>
              </li>
            );
          })}
        </ul>
      ) : (
        <p className="scan-screen__empty">
          {results.length === 0 && shaderpacks.length === 0 ? translate("scan.noResults") : translate("scan.noMatches")}
        </p>
      )}

      <footer className="scan-screen__actions">
        <PrimaryButton disabled={(selectedPaths.size === 0 && !selectedModpack) || creating} onClick={() => void createSelectedProject()}>
          {createProjectText(selectedPaths.size + Number(selectedModpack))}
        </PrimaryButton>
      </footer>
      {scanError && <p className="status-message" role="alert">{scanError.userMessage}</p>}
    </section>
  );
}
