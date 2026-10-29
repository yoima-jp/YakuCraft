import { useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import { SecondaryButton, TertiaryButton } from "../../components/button/Buttons";
import { Checkbox, Dialog } from "../../components/Controls";
import { ProgressBar } from "../../components/progress/ProgressBar";
import { cancelAnalysis, isShaderpackFolder } from "../../lib/tauri";
import { openFileDialog } from "../../lib/dialogs";
import { logFrontendError } from "../../lib/errorLogging";
import type { AppError } from "../../types/commands";
import { scanModsFolder, scanProjectInstanceSources } from "../scan/tauri";
import type {
  AddInstanceScanEventDto,
  ModLanguageStatusDto,
  ModScanResultDto,
  ShaderpackScanResultDto,
} from "../scan/types";
import { isSelectedByDefault } from "../scan/defaultSelection";
import "./AddUnitsDialog.css";

type Candidate = {
  path: string;
  fileName: string;
  label: string;
  version: string;
  kind: "jar" | "shaderpack";
  count: number;
  status: ModLanguageStatusDto;
};

const STATUS_LABELS: Record<ModLanguageStatusDto["kind"], string> = {
  untranslated: "未翻訳",
  partial: "部分翻訳",
  complete: "翻訳済み",
  no_language_files: "言語ファイルなし",
  failed: "読み込み失敗",
};

function candidateFromMod(result: ModScanResultDto): Candidate {
  const mod = result.mods[0];
  return {
    path: result.path,
    fileName: result.fileName,
    label: mod?.name ?? mod?.modId ?? result.fileName,
    version: mod?.version ?? "バージョン不明",
    kind: "jar",
    count: result.sourceKeyCount,
    status: result.status,
  };
}

function candidateFromShaderpack(result: ShaderpackScanResultDto): Candidate {
  return {
    path: result.path,
    fileName: result.fileName,
    label: "シェーダーパック",
    version: "",
    kind: "shaderpack",
    count: result.sourceKeyCount,
    status: result.status,
  };
}

function appError(error: unknown): AppError {
  void logFrontendError(error, "add mod instance scan");
  return error instanceof Error && "appError" in error
    ? (error as Error & { appError: AppError }).appError
    : { code: "unexpected", userMessage: "インスタンスをスキャンできませんでした。", recoverable: true };
}

function statusLabel(status: ModLanguageStatusDto): string {
  return status.kind === "partial"
    ? `部分翻訳 ${Math.round(status.coverage * 100)}%`
    : STATUS_LABELS[status.kind];
}

export function AddUnitsDialog({
  open,
  projectId,
  instancePath,
  busy,
  onClose,
  onAdd,
}: {
  open: boolean;
  projectId: string;
  instancePath: string | null;
  busy: boolean;
  onClose: () => void;
  onAdd: (paths: string[]) => void;
}) {
  const listRef = useRef<HTMLUListElement>(null);
  const scanGenerationRef = useRef(0);
  const scanJobRef = useRef<string | null>(null);
  const terminalJobsRef = useRef(new Set<string>());
  const [results, setResults] = useState<Candidate[] | null>(null);
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");
  const [scanError, setScanError] = useState<AppError | null>(null);
  const [scanProgress, setScanProgress] = useState<{ completed: number; total: number; currentFile: string } | null>(null);
  const [scanJobId, setScanJobId] = useState<string | null>(null);
  const [scanCancelRequested, setScanCancelRequested] = useState(false);
  const [pickerError, setPickerError] = useState<string | null>(null);

  const visibleCandidates = useMemo(() => {
    if (!results) return [];
    const query = search.trim().toLocaleLowerCase();
    return results.filter((candidate) => !query || [candidate.label, candidate.fileName]
      .some((value) => value.toLocaleLowerCase().includes(query)));
  }, [results, search]);

  // TanStack Virtual keeps instance scans with many mods responsive.
  // oxlint-disable-next-line react/incompatible-library
  const virtualizer = useVirtualizer({
    count: visibleCandidates.length,
    getScrollElement: () => listRef.current,
    getItemKey: (index) => visibleCandidates[index]?.path ?? index,
    estimateSize: () => 82,
    overscan: 8,
  });

  const handleScanEvent = (event: AddInstanceScanEventDto, generation: number) => {
    if (generation !== scanGenerationRef.current) return;
    if (event.kind === "progress") {
      setScanProgress(event.progress);
      return;
    }
    terminalJobsRef.current.add(event.jobId);
    scanJobRef.current = null;
    setScanJobId(null);
    setScanProgress(null);
    if (event.kind === "failed") {
      if (event.error.code !== "cancelled") setScanError(event.error);
      return;
    }
    const candidates = [
      ...event.results.map(candidateFromMod),
      ...event.shaderpacks.map(candidateFromShaderpack),
    ];
    setResults(candidates);
    setSelectedPaths(new Set(candidates
      .filter((candidate) => isSelectedByDefault(candidate.status, 100))
      .map((candidate) => candidate.path)));
  };

  const scanInstance = async (path?: string) => {
    if ((!path && !instancePath) || scanJobId || busy) return;
    setResults(null);
    setSelectedPaths(new Set());
    setSearch("");
    setPickerError(null);
    setScanError(null);
    setScanProgress(null);
    setScanCancelRequested(false);
    const generation = ++scanGenerationRef.current;
    const earlyEvents: AddInstanceScanEventDto[] = [];
    let jobId: string | null = null;
    try {
      const onEvent = (event: AddInstanceScanEventDto) => {
        if (jobId === null) earlyEvents.push(event);
        else handleScanEvent(event, generation);
      };
      const returnedJobId = path
        ? await scanModsFolder(path, onEvent)
        : await scanProjectInstanceSources(projectId, onEvent);
      jobId = returnedJobId;
      if (generation !== scanGenerationRef.current) {
        if (!terminalJobsRef.current.has(returnedJobId)) void cancelAnalysis(returnedJobId).catch(() => {});
        return;
      }
      if (!terminalJobsRef.current.has(returnedJobId)) {
        scanJobRef.current = returnedJobId;
        setScanJobId(returnedJobId);
      }
      for (const event of earlyEvents) handleScanEvent(event, generation);
    } catch (error: unknown) {
      if (generation === scanGenerationRef.current) setScanError(appError(error));
    }
  };

  const chooseFiles = async () => {
    setPickerError(null);
    try {
      const selected = await openFileDialog({
        multiple: true,
        directory: false,
        filters: [{ name: "JAR / シェーダー ZIP", extensions: ["jar", "zip"] }],
      });
      if (!selected) return;
      onClose();
      onAdd(typeof selected === "string" ? [selected] : selected);
    } catch (error: unknown) {
      void logFrontendError(error, "add mod file picker");
      setPickerError("ファイルを選べませんでした。もう一度お試しください。");
    }
  };

  const chooseFolder = async () => {
    setPickerError(null);
    try {
      const selected = await openFileDialog({ multiple: false, directory: true });
      const path = Array.isArray(selected) ? selected[0] : selected;
      if (!path) return;
      if (await isShaderpackFolder(path)) {
        onClose();
        onAdd([path]);
      } else {
        await scanInstance(path);
      }
    } catch (error: unknown) {
      void logFrontendError(error, "add folder picker");
      setPickerError("フォルダを選べませんでした。もう一度お試しください。");
    }
  };

  const cancelScan = async () => {
    if (!scanJobId) return;
    setScanCancelRequested(true);
    try {
      await cancelAnalysis(scanJobId);
    } catch (error: unknown) {
      setScanCancelRequested(false);
      setScanError(appError(error));
    }
  };

  const selectVisible = (selected: boolean) => {
    setSelectedPaths((current) => {
      const next = new Set(current);
      for (const candidate of visibleCandidates) {
        const selectable = candidate.status.kind !== "failed" && candidate.status.kind !== "no_language_files";
        if (!selectable) continue;
        if (selected) next.add(candidate.path);
        else next.delete(candidate.path);
      }
      return next;
    });
  };

  const close = () => {
    scanGenerationRef.current += 1;
    const jobId = scanJobRef.current;
    scanJobRef.current = null;
    setScanJobId(null);
    setScanProgress(null);
    setScanCancelRequested(false);
    if (jobId) void cancelAnalysis(jobId).catch(() => {});
    onClose();
  };

  return (
    <Dialog open={open} onClose={close} title="追加">
      <div className="add-units-dialog">
        {results === null && !scanProgress && !scanJobId && (
          <div className="add-units-dialog__sources">
            <SecondaryButton disabled={busy} onClick={() => void chooseFiles()}>ファイルを選ぶ</SecondaryButton>
            <SecondaryButton disabled={busy} onClick={() => void chooseFolder()}>フォルダを選ぶ</SecondaryButton>
            {instancePath && <SecondaryButton disabled={busy} onClick={() => void scanInstance()}>インスタンスから選ぶ</SecondaryButton>}
          </div>
        )}

        {(pickerError || scanError) && (
          <p className="add-units-dialog__error" role="alert">
            {pickerError ?? scanError?.userMessage}
          </p>
        )}

        {(scanProgress || scanJobId) && (
          <div className="add-units-dialog__progress">
            <ProgressBar
              value={scanProgress?.total ? Math.round(scanProgress.completed / scanProgress.total * 100) : 0}
              label="インスタンスのスキャン進行状況"
            />
            <output aria-live="polite">
              {scanProgress
                ? `${scanProgress.completed} / ${scanProgress.total} · ${scanProgress.currentFile}`
                : "準備中…"}
            </output>
            <SecondaryButton disabled={scanCancelRequested} onClick={() => void cancelScan()}>
              {scanCancelRequested ? "中止中…" : "キャンセル"}
            </SecondaryButton>
          </div>
        )}

        {results !== null && (
          <>
            <label className="add-units-dialog__search">
              <span className="visually-hidden">名前で検索</span>
              <input type="search" value={search} onChange={(event) => setSearch(event.currentTarget.value)} placeholder="名前で検索" />
            </label>
            <div className="add-units-dialog__selection">
              <output>{selectedPaths.size.toLocaleString("ja-JP")} 件選択中</output>
              <div>
                <TertiaryButton disabled={visibleCandidates.length === 0} onClick={() => selectVisible(true)}>表示中を全選択</TertiaryButton>
                <TertiaryButton disabled={selectedPaths.size === 0} onClick={() => selectVisible(false)}>表示中を全解除</TertiaryButton>
              </div>
            </div>
            {visibleCandidates.length > 0 ? (
              <ul className="add-units-dialog__list" ref={listRef} aria-label="追加できるModとシェーダーパック">
                <li aria-hidden="true" style={{ height: virtualizer.getTotalSize() }} />
                {virtualizer.getVirtualItems().map((row) => {
                  const candidate = visibleCandidates[row.index];
                  if (!candidate) return null;
                  const selectable = candidate.status.kind !== "failed" && candidate.status.kind !== "no_language_files";
                  const selected = selectedPaths.has(candidate.path);
                  return (
                    <li
                      className="add-units-dialog__row"
                      key={candidate.path}
                      data-selected={selected || undefined}
                      style={{ transform: `translateY(${row.start}px)`, height: row.size }}
                    >
                      <Checkbox
                        className="add-units-dialog__checkbox"
                        checked={selected}
                        disabled={!selectable}
                        onChange={(checked) => setSelectedPaths((current) => {
                          const next = new Set(current);
                          if (checked) next.add(candidate.path);
                          else next.delete(candidate.path);
                          return next;
                        })}
                      >
                        <span className="add-units-dialog__row-content">
                          <span className="add-units-dialog__heading">
                            <strong>{candidate.label}</strong>
                            {candidate.version && <span>{candidate.version}</span>}
                            <span className="add-units-dialog__status" data-status={candidate.status.kind}>
                              {statusLabel(candidate.status)}
                            </span>
                          </span>
                          <span className="add-units-dialog__filename">{candidate.fileName}</span>
                          {candidate.status.kind === "failed" && (
                            <span className="add-units-dialog__reason selectable">{candidate.status.reason}</span>
                          )}
                        </span>
                      </Checkbox>
                    </li>
                  );
                })}
              </ul>
            ) : (
              <p className="add-units-dialog__empty">
                {results.length === 0 ? "追加できるModやシェーダーパックがありません。" : "条件に合う項目がありません。"}
              </p>
            )}
            <div className="add-units-dialog__actions">
              <SecondaryButton onClick={() => { setResults(null); setSelectedPaths(new Set()); }}>戻る</SecondaryButton>
              <SecondaryButton
                disabled={selectedPaths.size === 0 || busy}
                onClick={() => {
                  const paths = results.filter((candidate) => selectedPaths.has(candidate.path)).map((candidate) => candidate.path);
                  close();
                  onAdd(paths);
                }}
              >
                {selectedPaths.size.toLocaleString("ja-JP")} 件を追加
              </SecondaryButton>
            </div>
          </>
        )}

        {!results && !scanProgress && !scanJobId && <div className="add-units-dialog__actions"><SecondaryButton onClick={close}>閉じる</SecondaryButton></div>}
      </div>
    </Dialog>
  );
}
