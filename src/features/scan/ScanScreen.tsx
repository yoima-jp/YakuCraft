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
import type { AnalysisChannelEvent, AppError } from "../../types/commands";
import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import { isAnalysisEventForJob } from "./analysisEvents";
import { defaultSelectedPaths } from "./defaultSelection";
import { useScanStore } from "./scanStore";
import { scanModsFolder } from "./tauri";
import type { ModLanguageStatusDto, ModScanResultDto, ModpackScanDto, ScanModsFolderEventDto } from "./types";
import "./ScanScreen.css";

const STATUS_LABELS: Record<ModLanguageStatusDto["kind"], string> = {
  untranslated: "未翻訳",
  partial: "部分翻訳",
  complete: "翻訳済み",
  no_language_files: "言語ファイルなし",
  failed: "読み込み失敗",
};

const FILTER_LABELS = [
  ["untranslated", "未翻訳"],
  ["partial", "部分翻訳"],
  ["complete", "翻訳済み"],
  ["no_language_files", "言語ファイルなし"],
  ["failed", "読み込み失敗"],
] as const satisfies ReadonlyArray<readonly [ModLanguageStatusDto["kind"], string]>;

type StatusFilters = Record<ModLanguageStatusDto["kind"], boolean>;

const DEFAULT_STATUS_FILTERS: StatusFilters = {
  untranslated: true,
  partial: true,
  complete: true,
  no_language_files: false,
  failed: false,
};

function appError(error: unknown): AppError {
  return error instanceof AppCommandError
    ? error.appError
    : { code: "unexpected", userMessage: "処理に失敗しました。もう一度お試しください。", recoverable: true };
}

function statusLabel(status: ModLanguageStatusDto): string {
  if (status.kind === "partial") return `部分翻訳 ${Math.round(status.coverage * 100)}%`;
  return STATUS_LABELS[status.kind];
}

function modName(result: ModScanResultDto): string {
  return result.mods[0]?.name ?? result.mods[0]?.modId ?? result.fileName;
}

function isSelectable(result: ModScanResultDto): boolean {
  return result.status.kind !== "no_language_files" && result.status.kind !== "failed";
}

function modpackHasImportableFiles(scan: ModpackScanDto): boolean {
  return scan.files.some((file) => file.locale.toLocaleLowerCase() === "en_us");
}

export function ScanScreen() {
  const folderPath = useScanStore((state) => state.folderPath);
  const scanRevision = useScanStore((state) => state.scanRevision);
  const scanJobId = useScanStore((state) => state.scanJobId);
  const scanProgress = useScanStore((state) => state.scanProgress);
  const results = useScanStore((state) => state.results);
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
        setScanResults(event.results, event.scannedFolder, event.instanceDir, event.looksLikeInstanceDir, event.modpackScan);
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
    if (results) setSelectedPaths(defaultSelectedPaths(results, thresholdPercent));
  }, [results, thresholdPercent]);

  useEffect(() => {
    setSelectedModpack(Boolean(modpackScan && modpackHasImportableFiles(modpackScan)));
  }, [results, modpackScan]);

  const visibleResults = useMemo(() => {
    if (!results) return [];
    const query = search.trim().toLocaleLowerCase();
    return results.filter((result) => {
      if (!statusFilters[result.status.kind]) return false;
      if (!query) return true;
      return [modName(result), result.fileName, result.mods[0]?.modId ?? ""]
        .some((value) => value.toLocaleLowerCase().includes(query));
    });
  }, [results, search, statusFilters]);

  const showModpack = Boolean(modpackScan);
  const modpackSelectable = Boolean(modpackScan && modpackHasImportableFiles(modpackScan));

  // TanStack Virtual はスクロール位置に合わせて可視行を差し替えるため、戻り値をReact Compilerでmemo化しない。
  // oxlint-disable-next-line react/incompatible-library
  const virtualizer = useVirtualizer({
    count: visibleResults.length + Number(showModpack),
    getScrollElement: () => listRef.current,
    estimateSize: (index) => (showModpack && index === 0 ? 104 : 88),
    getItemKey: (index) => showModpack && index === 0 ? "modpack" : visibleResults[index - Number(showModpack)]?.path ?? index,
    overscan: 8,
  });

  const visibleSelectable = visibleResults.filter(isSelectable);
  const allVisibleSelected = (visibleSelectable.length > 0 || (showModpack && modpackSelectable))
    && visibleSelectable.every((result) => selectedPaths.has(result.path))
    && (!showModpack || !modpackSelectable || selectedModpack);

  const updateVisibleSelection = useCallback((selected: boolean) => {
    setSelectedPaths((current) => {
      const next = new Set(current);
      for (const result of visibleSelectable) {
        if (selected) next.add(result.path);
        else next.delete(result.path);
      }
      return next;
    });
    if (showModpack && modpackSelectable) setSelectedModpack(selected);
  }, [modpackSelectable, showModpack, visibleSelectable]);

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
    const paths = results.filter((result) => selectedPaths.has(result.path)).map((result) => result.path);
    if (paths.length === 0 && !selectedModpack) return;

    const generation = ++createGenerationRef.current;
    setCreating(true);
    setCreationComplete(false);
    setAnalysisError(null);
    setAnalysisProgress(null);
    setAnalysisJobId(null);
    setModImportFailures([]);
    const selectedCount = paths.length + Number(selectedModpack);
    const sourceLabel = selectedModpack
      ? `${selectedCount.toLocaleString("ja-JP")} 件（Modpack 固有を含む）`
      : `${paths.length.toLocaleString("ja-JP")} 個のMod`;
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
          <h1 className="page-title" id="scan-title">フォルダを調べています</h1>
          {folderPath && <p className="scan-screen__path selectable">{folderPath}</p>}
          {scanError ? (
            <div className="scan-screen__error" role="alert">
              <p>{scanError.userMessage}</p>
              {scanError.detail && <pre className="selectable">{scanError.detail}</pre>}
              <SecondaryButton onClick={() => useScanStore.getState().retryScan()}>再スキャン</SecondaryButton>
            </div>
          ) : (
            <>
              <ProgressBar value={progressPercent} label="スキャンの進行状況" />
              <p className="analyze-screen__stage" aria-live="polite">
                {scanProgress
                  ? `${scanProgress.completed} / ${scanProgress.total} · ${scanProgress.currentFile}`
                  : "準備中…"}
              </p>
              {scanJobId && (
                <SecondaryButton disabled={scanCancelRequested} onClick={() => void cancelFolderScan()}>
                  {scanCancelRequested ? "中止中…" : "キャンセル"}
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
          <h1 className="page-title" id="scan-results-title">日本語訳を確認</h1>
          <p className="scan-screen__path selectable">{scannedFolder ?? folderPath}</p>
        </div>
        {looksLikeInstanceDir ? null : (
          <p className="scan-screen__instance-note">
            インスタンスのフォルダが見つからないため、リソースパックの直接配置と jar の上書きは使えません
          </p>
        )}
      </header>

      <div className="scan-screen__controls">
        <label className="scan-screen__search">
          <span>名前で検索</span>
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
          <span>部分翻訳の対象</span>
          <span className="scan-screen__threshold-input">
            <NumberInput
              aria-label="部分翻訳のしきい値"
              min={0}
              max={100}
              value={thresholdInput}
              onChange={(event) => setThresholdInput(event.currentTarget.value)}
            />
            <span>% 未満</span>
          </span>
        </div>
      </div>

      <fieldset className="scan-screen__filters">
        <legend>状態</legend>
        {FILTER_LABELS.map(([kind, label]) => (
          <Checkbox
            checked={statusFilters[kind]}
            key={kind}
            onChange={(checked) => setStatusFilters((current) => ({ ...current, [kind]: checked }))}
          >
            {label}
          </Checkbox>
        ))}
      </fieldset>

      <div className="scan-screen__selection-bar">
        <output aria-live="polite">{(selectedPaths.size + Number(selectedModpack)).toLocaleString("ja-JP")} 件選択中</output>
        <div>
          <TertiaryButton disabled={(visibleSelectable.length === 0 && !(showModpack && modpackSelectable)) || allVisibleSelected} onClick={() => updateVisibleSelection(true)}>
            表示中を全選択
          </TertiaryButton>
          <TertiaryButton disabled={(visibleSelectable.length === 0 && !(showModpack && modpackSelectable)) || (selectedPaths.size === 0 && !selectedModpack)} onClick={() => updateVisibleSelection(false)}>
            表示中を全解除
          </TertiaryButton>
        </div>
      </div>

      {visibleResults.length > 0 || showModpack ? (
        <ul
          className="scan-screen__list"
          ref={listRef}
          aria-label="スキャンしたModとModpackファイル"
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
                        <strong>Modpack 固有（クエスト・KubeJS）</strong>
                        <span>{modpackScan.totalEntries.toLocaleString("ja-JP")} 件</span>
                        <span className="scan-screen__status" data-status={oldFtb ? "partial" : "untranslated"}>
                          {oldFtb ? "クエスト旧形式" : "翻訳対象"}
                        </span>
                      </span>
                      <span className="scan-screen__filename">
                        クエスト {modpackScan.ftbQuestEntries.toLocaleString("ja-JP")} 件 · KubeJS {modpackScan.kubejsEntries.toLocaleString("ja-JP")} 件
                      </span>
                      {oldFtb && (
                        <span className="scan-screen__reason">
                          この形式のクエストは翻訳できません{modpackScan.kubejsNamespaces.length > 0 ? "。KubeJS は取り込めます" : ""}
                        </span>
                      )}
                    </span>
                  </Checkbox>
                </li>
              );
            }
            const resultIndex = virtualRow.index - Number(showModpack);
            const result = visibleResults[resultIndex];
            if (!result) return null;
            const selectable = isSelectable(result);
            const label = modName(result);
            const version = result.mods[0]?.version ?? "バージョン不明";
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
                      <span className="scan-screen__status" data-status={result.status.kind}>{statusLabel(result.status)}</span>
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
          {results.length === 0 ? "mod が見つかりません。" : "条件に合うModがありません。"}
        </p>
      )}

      <footer className="scan-screen__actions">
        <PrimaryButton disabled={(selectedPaths.size === 0 && !selectedModpack) || creating} onClick={() => void createSelectedProject()}>
          {(selectedPaths.size + Number(selectedModpack)).toLocaleString("ja-JP")} 件でプロジェクトを作成
        </PrimaryButton>
      </footer>
      {scanError && <p className="status-message" role="alert">{scanError.userMessage}</p>}
    </section>
  );
}
