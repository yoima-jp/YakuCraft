import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ChangeEvent, CSSProperties, KeyboardEvent, MouseEvent, PointerEvent } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import type { EditorColumn, EditorFilter, FocusedCell } from "../../stores/editorStore";
import { useEditorStore } from "../../stores/editorStore";
import type { EntryFlag, TranslationEntry } from "../../types/translation";
import { isImeCompositionKeyDown } from "../../lib/keyboard";
import { useSettingsStore } from "../../stores/settingsStore";
import { useProjectStore } from "../../stores/projectStore";
import {
  ContextMenu,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
  ContextMenuShortcut,
  shouldPreserveNativeContextMenu,
  useContextMenuTrigger,
} from "../../components/ContextMenu";
import { Tooltip } from "../../components/Controls";
import {
  clearVisiblePreviewThumbnails,
  isPreviewableEntry,
  setBackgroundPreviewThumbnails,
  setVisiblePreviewThumbnails,
  usePreviewThumbnail,
} from "../preview/thumbnails";
import {
  calculateResponsiveColumnWidths,
  DEFAULT_COLUMN_WIDTHS,
  MAX_COLUMN_WIDTH,
  MIN_COLUMN_WIDTHS,
  normalizeColumnWidths,
  readUserSizedColumns,
  storeColumnWidths,
  type TableColumnId,
} from "./tableLayout";
import { prepareRowContextSelection } from "./rowContextMenu";
import { registerPendingDraftCommit } from "./pendingDrafts";

/* oxlint-disable jsx-a11y/prefer-tag-over-role -- 仮想ARIA gridの構造と、操作可能な幅調整separatorをdivで表現する。 */

const HEADER_HEIGHT = 42;
const OVERSCAN = 8;
// CSS側の末尾余白（44pxのクリック領域 + 24pxのspacing-xl）と合わせ、バーが最終行を覆わない。
const SELECTION_TOOLBAR_SCROLL_PADDING = 68;

const ORIGIN_LABELS: Record<TranslationEntry["origin"], string> = {
  missing: "未翻訳",
  existing: "既存訳",
  ai: "AI",
  edited: "編集済み",
};

const FLAG_LABELS: Record<EntryFlag, string> = {
  source_fallback: "原文補完",
  needs_review: "要確認",
  placeholder_error: "置換文字列の警告",
  malformed_source: "原文形式エラー",
  asset_ambiguous: "関連するアセット候補が複数あります",
  asset_missing: "関連するアセットがありません",
};

export const FILTER_OPTIONS: Array<[EditorFilter, string]> = [
  ["all", "すべて"],
  ["missing", "未翻訳"],
  ["existing", "既存訳"],
  ["ai", "AI"],
  ["edited", "編集済み"],
  ["needs_review", "要確認"],
  ["source_fallback", "原文補完"],
  ["placeholder_error", "置換文字列の警告"],
  ["asset_missing", "関連するアセットなし"],
  ["glossary_mismatch", "用語集と異なる"],
];

const ORIGIN_MARKS: Record<TranslationEntry["origin"], string> = {
  missing: "○",
  existing: "✓",
  ai: "",
  edited: "✎",
};

interface TranslationTableProps {
  onCopy: () => void;
  onCopyEntries?: (entryIds: number[], column: "key" | "source" | "target") => void;
  onTranslateEntries?: (entryIds: number[]) => void;
  onAddGlossary?: (sourceTerm: string, targetTerm: string) => void;
  aiJobActive?: boolean;
  onPaste: () => void;
}

interface TableColumn {
  id: TableColumnId;
  label: string;
}

export function TranslationTable({
  onCopy,
  onCopyEntries = () => undefined,
  onTranslateEntries = () => undefined,
  onAddGlossary = () => undefined,
  aiJobActive = false,
  onPaste,
}: TranslationTableProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const focusSyncFrameRef = useRef<number | null>(null);
  const previousModUnitIdRef = useRef(useEditorStore.getState().activeModUnitId);
  const activeModUnitId = useEditorStore((state) => state.activeModUnitId);
  const hasMultipleNamespaces = useEditorStore((state) => state.namespaces.length > 1);
  const orderedIds = useEditorStore((state) => state.orderedIds);
  const filteredIds = useEditorStore((state) => state.filteredIds);
  const hasSelection = useEditorStore((state) => state.selectedIds.length > 0);
  const focusedCell = useEditorStore((state) => state.focusedCell);
  const setFocusedCell = useEditorStore((state) => state.setFocusedCell);
  const setScrollTop = useEditorStore((state) => state.setScrollTop);
  const editEntry = useEditorStore((state) => state.editEntry);
  const selectEntry = useEditorStore((state) => state.selectEntry);
  const rowDensity = useSettingsStore((state) => state.settings["editor.rowDensity"]);
  const storedColumnWidths = useSettingsStore((state) => state.settings["editor.tableColumnWidths"]);
  const wrapSource = useSettingsStore((state) => state.settings["editor.wrapSource"]);
  const wrapTarget = useSettingsStore((state) => state.settings["editor.wrapTarget"]);
  const saveSettings = useSettingsStore((state) => state.save);
  const projectId = useProjectStore((state) => state.details?.project.id ?? "");
  const [columnWidths, setColumnWidths] = useState<Record<TableColumnId, number>>(DEFAULT_COLUMN_WIDTHS);
  const columnWidthsRef = useRef(columnWidths);
  const [userSizedColumns, setUserSizedColumns] = useState<ReadonlySet<TableColumnId>>(() => new Set());
  const userSizedColumnsRef = useRef<ReadonlySet<TableColumnId>>(new Set());
  const [viewportWidth, setViewportWidth] = useState(0);
  const rowHeight = rowDensity === "compact" ? 36 : rowDensity === "spacious" ? 56 : 44;

  useEffect(() => {
    const widths = normalizeColumnWidths(storedColumnWidths);
    const userSized = readUserSizedColumns(storedColumnWidths);
    columnWidthsRef.current = widths;
    userSizedColumnsRef.current = userSized;
    setColumnWidths(widths);
    setUserSizedColumns(userSized);
  }, [storedColumnWidths]);

  useLayoutEffect(() => {
    const scrollElement = scrollRef.current;
    if (!scrollElement) return;
    const updateWidth = () => {
      const width = scrollElement.clientWidth;
      setViewportWidth((current) => current === width ? current : width);
    };
    updateWidth();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", updateWidth);
      return () => window.removeEventListener("resize", updateWidth);
    }
    const observer = new ResizeObserver(updateWidth);
    observer.observe(scrollElement);
    return () => observer.disconnect();
  }, []);

  const updateColumnWidths = useCallback((widths: Record<TableColumnId, number>) => {
    columnWidthsRef.current = widths;
    setColumnWidths(widths);
  }, []);

  const persistColumnWidths = useCallback(() => {
    // Inspectorの開閉で伸縮した表示幅は保存せず、ユーザーが決めた幅だけを次回の基準にする。
    void saveSettings({
      "editor.tableColumnWidths": storeColumnWidths(columnWidthsRef.current, userSizedColumnsRef.current),
    }).catch(() => undefined);
  }, [saveSettings]);

  const tableColumns = useMemo<TableColumn[]>(
    () => [
      { id: "key", label: "キー" },
      { id: "source", label: "原文" },
      { id: "target", label: "日本語訳" },
      { id: "state", label: "状態" },
      ...(hasMultipleNamespaces ? [{ id: "namespace" as const, label: "名前空間" }] : []),
    ],
    [hasMultipleNamespaces],
  );
  const gridColumns = useMemo<TableColumn[]>(
    () => [{ id: "preview", label: "プレビュー" }, ...tableColumns],
    [tableColumns],
  );
  const visibleColumnIds = gridColumns.map(({ id }) => id);
  const preferredTableWidth = visibleColumnIds.reduce((sum, id) => sum + columnWidths[id], 0);
  const responsiveColumnWidths = calculateResponsiveColumnWidths(
    viewportWidth || preferredTableWidth,
    columnWidths,
    userSizedColumns,
    visibleColumnIds,
  );
  const gridTemplateColumns = gridColumns.map(({ id }) => `${responsiveColumnWidths[id]}px`).join(" ");
  const tableWidth = visibleColumnIds.reduce((sum, id) => sum + responsiveColumnWidths[id], 0);

  // TanStack Virtual の hook はCompilerのmemo化対象外だが、5万行をDOM化しない要件に必要。
  // oxlint-disable-next-line react/incompatible-library
  const virtualizer = useVirtualizer({
    count: filteredIds.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    scrollPaddingEnd: hasSelection ? SELECTION_TOOLBAR_SCROLL_PADDING : 0,
    overscan: OVERSCAN,
    initialRect: { width: 1024, height: 520 },
  });

  useLayoutEffect(() => {
    // 幅や折り返し設定の変更で行高が変わるため、仮想行の測定値と後続行の位置を破棄する。
    virtualizer.measure();
  }, [gridTemplateColumns, rowHeight, virtualizer, wrapSource, wrapTarget]);

  useLayoutEffect(() => {
    if (previousModUnitIdRef.current === activeModUnitId) return;
    previousModUnitIdRef.current = activeModUnitId;
    const scrollElement = scrollRef.current;
    if (!scrollElement) return;
    scrollElement.scrollTop = useEditorStore.getState().scrollTop;
    virtualizer.measure();
  }, [activeModUnitId, virtualizer]);

  const focusCell = useCallback(
    (entryId: number, column: EditorColumn) => {
      setFocusedCell({ entryId, column });
      const index = filteredIds.indexOf(entryId);
      if (index < 0) return;
      virtualizer.scrollToIndex(index, { align: "auto" });
      requestAnimationFrame(() => {
        scrollRef.current
          ?.querySelector<HTMLElement>(`[data-entry-id="${entryId}"][data-column="${column}"]`)
          ?.focus();
      });
    },
    [filteredIds, setFocusedCell, virtualizer],
  );

  const moveFocus = useCallback(
    (rowIndex: number, columnIndex: number) => {
      if (filteredIds.length === 0) return;
      const boundedRow = Math.min(filteredIds.length - 1, Math.max(0, rowIndex));
      const boundedColumn = Math.min(tableColumns.length - 1, Math.max(0, columnIndex));
      const entryId = filteredIds[boundedRow];
      const column = tableColumns[boundedColumn]?.id;
      if (entryId === undefined || !column || column === "preview") return;
      focusCell(entryId, column);
    },
    [filteredIds, focusCell, tableColumns],
  );

  const moveToTarget = useCallback(
    (rowIndex: number) => {
      const targetColumn = tableColumns.findIndex((column) => column.id === "target");
      moveFocus(rowIndex, targetColumn);
    },
    [moveFocus, tableColumns],
  );

  const syncFocusedRowToViewport = useCallback(() => {
    if (focusSyncFrameRef.current !== null) return;
    // スクロールイベント時点では仮想行のDOM更新が終わっていないことがあるため、次フレームにまとめて判定する。
    focusSyncFrameRef.current = requestAnimationFrame(() => {
      focusSyncFrameRef.current = null;
      const scrollElement = scrollRef.current;
      const currentFocus = useEditorStore.getState().focusedCell;
      if (!scrollElement || !currentFocus) return;

      const scrollBounds = scrollElement.getBoundingClientRect();
      const visibleTop = scrollBounds.top + scrollElement.clientTop + HEADER_HEIGHT;
      const visibleBottom = scrollBounds.top + scrollElement.clientTop + scrollElement.clientHeight;
      const visibleRows = Array.from(
        scrollElement.querySelectorAll<HTMLElement>('[role="row"][data-entry-id]'),
      ).filter((row) => {
        const bounds = row.getBoundingClientRect();
        return bounds.bottom > visibleTop && bounds.top < visibleBottom;
      });
      const focusedRow = visibleRows.find((row) => Number(row.dataset.entryId) === currentFocus.entryId);
      if (focusedRow || visibleRows.length === 0) return;

      // 手動スクロールでfocusedCellが仮想DOMから外れると次の矢印移動が古い行を基準にするため、表示中の行へ同期する。
      const nextRow = visibleRows[0];
      if (!nextRow) return;
      const nextEntryId = Number(nextRow.dataset.entryId);
      if (!Number.isSafeInteger(nextEntryId)) return;
      const cell = nextRow.querySelector<HTMLElement>(`[data-column="${currentFocus.column}"]`)
        ?? nextRow.querySelector<HTMLElement>('[data-column="target"]')
        ?? nextRow.querySelector<HTMLElement>("[role=gridcell]");
      const nextColumn = cell?.dataset.column as EditorColumn | undefined;
      if (!nextColumn) return;

      const activeElement = document.activeElement;
      const restoreTableFocus = activeElement === document.body || activeElement === document.documentElement ||
        (activeElement instanceof HTMLElement && activeElement.closest(".editor-table") !== null);
      setFocusedCell({ entryId: nextEntryId, column: nextColumn });
      if (!restoreTableFocus) return;

      requestAnimationFrame(() => {
        const active = document.activeElement;
        if (active !== document.body && active !== document.documentElement &&
          !(active instanceof HTMLElement && active.closest(".editor-table"))) return;
        scrollElement
          .querySelector<HTMLElement>(`[data-entry-id="${nextEntryId}"][data-column="${nextColumn}"]`)
          ?.focus({ preventScroll: true });
      });
    });
  }, [setFocusedCell]);

  const queueVisibleThumbnails = useCallback(() => {
    const scrollElement = scrollRef.current;
    if (!scrollElement || !projectId) {
      clearVisiblePreviewThumbnails();
      return;
    }
    if (filteredIds.length === 0) {
      clearVisiblePreviewThumbnails();
      return;
    }
    // scrollイベント中はVirtualizerの再描画前で可視行が古いことがあるため、固定行高から範囲を即時計算する。
    const firstIndex = Math.min(filteredIds.length - 1, Math.floor(scrollElement.scrollTop / rowHeight));
    const visibleEnd = Math.max(0, scrollElement.scrollTop + scrollElement.clientHeight - HEADER_HEIGHT);
    const endIndex = Math.min(filteredIds.length, Math.max(firstIndex + 1, Math.ceil(visibleEnd / rowHeight)));
    const jobs: TranslationEntry[] = [];
    for (let index = firstIndex; index < endIndex; index += 1) {
      const entryId = filteredIds[index];
      const entry = entryId === undefined ? undefined : useEditorStore.getState().entries.get(entryId);
      if (entry) jobs.push(entry);
    }
    setVisiblePreviewThumbnails(jobs, projectId);
  }, [filteredIds, projectId, rowHeight]);

  const handleScroll = useCallback(() => {
    const scrollTop = scrollRef.current?.scrollTop;
    if (scrollTop !== undefined && useEditorStore.getState().scrollTop !== scrollTop) {
      setScrollTop(scrollTop);
    }
    syncFocusedRowToViewport();
    queueVisibleThumbnails();
  }, [queueVisibleThumbnails, setScrollTop, syncFocusedRowToViewport]);

  useEffect(() => {
    queueVisibleThumbnails();
  }, [filteredIds, projectId, queueVisibleThumbnails]);

  useEffect(() => {
    const state = useEditorStore.getState();
    const entries = orderedIds.flatMap((entryId) => {
      const entry = state.entries.get(entryId);
      return entry ? [entry] : [];
    });
    setBackgroundPreviewThumbnails(entries, projectId);
  }, [orderedIds, projectId]);

  useEffect(() => () => {
    if (focusSyncFrameRef.current !== null) cancelAnimationFrame(focusSyncFrameRef.current);
    clearVisiblePreviewThumbnails();
    setBackgroundPreviewThumbnails([], "");
  }, []);

  const resizeColumn = (columnId: TableColumnId, width: number) => {
    const nextWidths = { ...columnWidthsRef.current, [columnId]: Math.min(MAX_COLUMN_WIDTH, Math.max(MIN_COLUMN_WIDTHS[columnId], width)) };
    const nextUserSizedColumns = new Set(userSizedColumnsRef.current);
    nextUserSizedColumns.add(columnId);
    userSizedColumnsRef.current = nextUserSizedColumns;
    setUserSizedColumns(nextUserSizedColumns);
    updateColumnWidths(nextWidths);
    return nextWidths;
  };

  const autoFitColumn = (columnId: TableColumnId) => {
    if (columnId === "preview") {
      resizeColumn(columnId, DEFAULT_COLUMN_WIDTHS.preview);
      persistColumnWidths();
      return;
    }
    const state = useEditorStore.getState();
    const fontSize = Number(useSettingsStore.getState().settings["editor.fontSize"] ?? 13);
    let maxTextWidth = estimateTextWidth(tableColumns.find(({ id }) => id === columnId)?.label ?? "", fontSize, MAX_COLUMN_WIDTH - 32);
    for (const id of state.orderedIds) {
      const entry = state.entries.get(id);
      if (!entry) continue;
      const value = columnId === "key"
        ? entry.key
        : columnId === "source"
          ? entry.sourceText ?? ""
          : columnId === "target"
            ? entry.targetText
            : columnId === "namespace"
              ? entry.namespace
              : `${ORIGIN_LABELS[entry.origin]} ${entry.flags.length ? "!" : ""}`;
      const measured = estimateTextWidth(value, fontSize, MAX_COLUMN_WIDTH - 32);
      maxTextWidth = Math.max(maxTextWidth, measured);
      if (maxTextWidth >= MAX_COLUMN_WIDTH - 32) break;
    }
    resizeColumn(columnId, Math.ceil(maxTextWidth + 32));
    persistColumnWidths();
  };

  const handleGridKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const target = event.target;
    if (
      target instanceof HTMLElement &&
      (target.matches("input, textarea, select, button") || target.isContentEditable)
    ) {
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key === "Tab") return;

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "c") {
      event.preventDefault();
      onCopy();
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "v") {
      event.preventDefault();
      onPaste();
      return;
    }

    const currentId = focusedCell?.entryId ?? filteredIds[0];
    const currentIndex = currentId === undefined ? 0 : filteredIds.indexOf(currentId);
    const currentColumnIndex = Math.max(
      0,
      tableColumns.findIndex((column) => column.id === (focusedCell?.column ?? "target")),
    );
    const pageSize = Math.max(1, Math.floor(((scrollRef.current?.clientHeight ?? rowHeight) - HEADER_HEIGHT) / rowHeight));

    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      moveFocus(currentIndex + (event.key === "ArrowDown" ? 1 : -1), currentColumnIndex);
    } else if (event.key === "PageDown" || event.key === "PageUp") {
      event.preventDefault();
      moveFocus(currentIndex + (event.key === "PageDown" ? pageSize : -pageSize), currentColumnIndex);
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      moveFocus(event.key === "Home" ? 0 : filteredIds.length - 1, currentColumnIndex);
    } else if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      event.preventDefault();
      moveFocus(currentIndex, currentColumnIndex + (event.key === "ArrowRight" ? 1 : -1));
    } else if (event.key === "Tab") {
      const nextIndex = currentIndex + (event.shiftKey ? -1 : 1);
      if (nextIndex >= 0 && nextIndex < filteredIds.length) {
        event.preventDefault();
        moveToTarget(nextIndex);
      }
    } else if (event.key === " " && focusedCell) {
      event.preventDefault();
      const mode = event.shiftKey ? "range" : event.ctrlKey || event.metaKey ? "toggle" : "replace";
      selectEntry(focusedCell.entryId, mode);
    } else if (event.key === "Escape") {
      if (useEditorStore.getState().selectedIds.length > 0) {
        event.preventDefault();
        event.stopPropagation();
        useEditorStore.getState().clearSelection();
      }
    }
  };

  const virtualRows = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();

  return (
    <>
      <span id="translation-table-shortcuts" className="visually-hidden">
        矢印でセルを移動、PageUp / PageDownでページ移動、Home / Endで先頭 / 末尾へ移動、Spaceで行を選択、EnterまたはF2で編集、Tabで次の訳へ移動、Shift+Enterで改行、Ctrl+Cでコピー、Ctrl+Vで貼り付け、Ctrl+Zで元に戻す、Ctrl+Yでやり直す、Escapeで選択解除。
      </span>
      <div
        className="editor-table"
        role="grid"
        aria-label="翻訳一覧"
        aria-describedby="translation-table-shortcuts"
        aria-rowcount={filteredIds.length + 1}
        aria-colcount={gridColumns.length}
        aria-keyshortcuts="ArrowUp ArrowDown ArrowLeft ArrowRight PageUp PageDown Home End Enter F2 Tab Shift+Tab Space Control+Space Shift+Space Control+C Control+V Control+Z Control+Y Escape"
        tabIndex={filteredIds.length === 0 || focusedCell === null ? 0 : -1}
        onKeyDown={handleGridKeyDown}
      >
      <div className={`editor-table__scroll${hasSelection ? " editor-table__scroll--selection-padding" : ""}`} ref={scrollRef} onScroll={handleScroll}>
        <div
          className={`editor-table__content${hasSelection ? " editor-table__content--selection-padding" : ""}`}
          role="rowgroup"
          style={{ height: totalSize + HEADER_HEIGHT, minWidth: tableWidth, width: tableWidth }}
        >
          <div
            className="editor-table__header"
            role="row"
            aria-rowindex={1}
            style={{ gridTemplateColumns: gridTemplateColumns, width: tableWidth }}
          >
            {gridColumns.map((column, index) => (
              <div
                className={`editor-table__heading${column.id === "preview" ? " editor-table__heading--preview" : ""}`}
                role="columnheader"
                aria-label={column.label || "プレビュー"}
                aria-colindex={index + 1}
                key={column.id}
              >
                {column.label}
                <ColumnResizeHandle
                  column={column}
                  width={responsiveColumnWidths[column.id]}
                  onResize={(width) => resizeColumn(column.id, width)}
                  onCommit={persistColumnWidths}
                  onAutoFit={() => autoFitColumn(column.id)}
                />
              </div>
            ))}
          </div>
          {virtualRows.map((virtualRow) => {
            const entryId = filteredIds[virtualRow.index];
            if (entryId === undefined) return null;
            return (
              <EditorRow
                key={entryId}
                entryId={entryId}
                index={virtualRow.index}
                columns={tableColumns}
                gridTemplateColumns={gridTemplateColumns}
                tableWidth={tableWidth}
                rowStart={virtualRow.start + HEADER_HEIGHT}
                rowHeight={virtualRow.size}
                measureElement={virtualizer.measureElement}
                projectId={projectId}
                onSelect={selectEntry}
                onCopyEntries={onCopyEntries}
                onTranslateEntries={onTranslateEntries}
                onAddGlossary={onAddGlossary}
                aiJobActive={aiJobActive}
                onMoveToTarget={moveToTarget}
                editEntry={editEntry}
                setFocusedCell={setFocusedCell}
              />
            );
          })}
          {filteredIds.length === 0 && (
            <output className="editor-table__empty" aria-live="polite">
              該当する項目はありません
            </output>
          )}
        </div>
      </div>
      </div>
    </>
  );
}

function ColumnResizeHandle({
  column,
  width,
  onResize,
  onCommit,
  onAutoFit,
}: {
  column: TableColumn;
  width: number;
  onResize: (width: number) => Record<TableColumnId, number>;
  onCommit: () => void;
  onAutoFit: () => void;
}) {
  const drag = useRef<{ pointerId: number; x: number; width: number } | null>(null);

  const resize = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    onResize(drag.current.width + event.clientX - drag.current.x);
  };

  const finish = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    drag.current = null;
    onCommit();
  };

  return (
    <div
      className="editor-table__resize-handle"
      role="separator"
      aria-label={`${column.label}列の幅`}
      aria-orientation="vertical"
      aria-valuemin={MIN_COLUMN_WIDTHS[column.id]}
      aria-valuemax={MAX_COLUMN_WIDTH}
      aria-valuenow={width}
      aria-valuetext={`${width} ピクセル`}
      aria-keyshortcuts="Alt+ArrowLeft Alt+ArrowRight"
      tabIndex={0}
      onPointerDown={(event) => {
        event.preventDefault();
        event.stopPropagation();
        drag.current = { pointerId: event.pointerId, x: event.clientX, width };
        event.currentTarget.setPointerCapture?.(event.pointerId);
      }}
      onPointerMove={resize}
      onPointerUp={finish}
      onPointerCancel={finish}
      onDoubleClick={(event) => {
        event.preventDefault();
        onAutoFit();
      }}
      onKeyDown={(event) => {
        if (!event.altKey || (event.key !== "ArrowLeft" && event.key !== "ArrowRight")) return;
        event.preventDefault();
        event.stopPropagation();
        const direction = event.key === "ArrowRight" ? 1 : -1;
        onResize(width + direction * 16);
        onCommit();
      }}
    />
  );
}

interface EditorRowProps {
  entryId: number;
  index: number;
  columns: TableColumn[];
  gridTemplateColumns: string;
  tableWidth: number;
  rowStart: number;
  rowHeight: number;
  measureElement: (node: HTMLDivElement | null) => void;
  projectId: string;
  onSelect: (entryId: number, mode?: "replace" | "toggle" | "range") => void;
  onCopyEntries: (entryIds: number[], column: "key" | "source" | "target") => void;
  onTranslateEntries: (entryIds: number[]) => void;
  onAddGlossary: (sourceTerm: string, targetTerm: string) => void;
  aiJobActive: boolean;
  onMoveToTarget: (rowIndex: number) => void;
  editEntry: (entryId: number, targetText: string) => void;
  setFocusedCell: (cell: FocusedCell | null) => void;
}

const EditorRow = memo(function EditorRow({
  entryId,
  index,
  columns,
  gridTemplateColumns,
  tableWidth,
  rowStart,
  rowHeight,
  measureElement,
  projectId,
  onSelect,
  onCopyEntries,
  onTranslateEntries,
  onAddGlossary,
  aiJobActive,
  onMoveToTarget,
  editEntry,
  setFocusedCell,
}: EditorRowProps) {
  const entry = useEditorStore((state) => state.entries.get(entryId));
  const selected = useEditorStore((state) => state.selectedIdSet.has(entryId));
  const aiHighlighted = useEditorStore((state) => state.aiHighlightedIds.has(entryId));
  const focusedCell = useEditorStore((state) => state.focusedCell);
  const contextMenu = useContextMenuTrigger();
  const [contextEntryIds, setContextEntryIds] = useState<number[]>([]);
  // 入力ごとにZustandを更新すると可視行の購読が一斉に走るため、下書きは編集中の行だけに置く。
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [editorOverlay, setEditorOverlay] = useState<{ left: number; top: number; anchorTop: number; width: number } | null>(null);
  const editingRef = useRef(false);
  const draftRef = useRef("");
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const editorOverlayRef = useRef<HTMLDivElement>(null);
  const cancelledRef = useRef(false);
  const movingEditorRef = useRef(false);
  const focusRestoreRequestRef = useRef(0);
  const selectionRef = useRef<[number, number] | null>(null);

  const scheduleCellFocusRestore = useCallback(() => {
    const requestId = ++focusRestoreRequestRef.current;
    requestAnimationFrame(() => {
      // Escape直後に再編集した場合、古い復元処理で新しい入力欄からフォーカスを奪わない。
      if (requestId !== focusRestoreRequestRef.current || editingRef.current) return;
      document
        .querySelector<HTMLElement>(`[data-entry-id="${entryId}"][data-column="target"]`)
        ?.focus();
    });
  }, [entryId]);

  useEffect(() => {
    if (!editing) return;
    requestAnimationFrame(() => {
      const input = inputRef.current;
      input?.focus();
      const range = selectionRef.current ?? [input?.value.length ?? 0, input?.value.length ?? 0];
      input?.setSelectionRange(range[0], range[1]);
      if (editorOverlay && input) resizeExpandedEditor(input);
      movingEditorRef.current = false;
    });
  }, [editing, editorOverlay]);

  useLayoutEffect(() => {
    if (!editing || editorOverlay) return;
    const input = inputRef.current;
    if (!input || !textareaNeedsExpansion(input)) return;
    const cell = document.querySelector<HTMLElement>(`[data-entry-id="${entryId}"][data-column="target"]`);
    // 以前の文字幅推定では実フォントやセル内折り返しによる縦overflowを判定できなかった。
    // DOMに出たtextareaの寸法を使い、編集開始直後も入力で折り返した直後も描画前に拡大する。
    movingEditorRef.current = true;
    setEditorOverlay(editorOverlayPosition(cell));
  }, [draft, editing, editorOverlay, entryId]);

  useLayoutEffect(() => {
    if (!editing || !editorOverlay) return;
    const overlay = editorOverlayRef.current;
    if (!overlay) return;
    if (inputRef.current) resizeExpandedEditor(inputRef.current);

    // 実際の高さで判定し、下端に収まる間はセル位置を保つ。入力が伸びて切れる場合だけ上へ寄せる。
    const maxTop = window.innerHeight - overlay.getBoundingClientRect().height - 16;
    const top = Math.max(16, Math.min(editorOverlay.anchorTop, maxTop));
    if (top !== editorOverlay.top) {
      setEditorOverlay((current) => current && current.anchorTop === editorOverlay.anchorTop
        ? { ...current, top }
        : current);
    }
  }, [draft, editing, editorOverlay]);

  const commit = useCallback((restoreFocus = false) => {
    if (!editingRef.current) return;
    const currentDraft = draftRef.current;
    if (!cancelledRef.current) editEntry(entryId, currentDraft);
    editingRef.current = false;
    draftRef.current = currentDraft;
    setEditing(false);
    setEditorOverlay(null);
    setFocusedCell({ entryId, column: "target" });
    if (restoreFocus) scheduleCellFocusRestore();
  }, [editEntry, entryId, scheduleCellFocusRestore, setFocusedCell]);

  useEffect(() => () => {
    // 仮想化で編集中の行が外れる場合も、通常の確定処理を通して下書きを保持する。
    if (editingRef.current) commit();
  }, [commit]);

  useEffect(() => {
    if (!editing) return;
    return registerPendingDraftCommit(() => commit());
  }, [commit, editing]);

  if (!entry) return null;

  const beginEdit = (_event?: { currentTarget: HTMLElement }, initialText = entry.targetText) => {
    focusRestoreRequestRef.current += 1;
    cancelledRef.current = false;
    editingRef.current = true;
    draftRef.current = initialText;
    selectionRef.current = [initialText.length, initialText.length];
    setEditorOverlay(null);
    setDraft(initialText);
    setEditing(true);
    setFocusedCell({ entryId, column: "target" });
  };

  const cancel = () => {
    cancelledRef.current = true;
    editingRef.current = false;
    setEditing(false);
    setEditorOverlay(null);
    setFocusedCell({ entryId, column: "target" });
    scheduleCellFocusRestore();
  };

  const focusColumn = (column: EditorColumn) => {
    setFocusedCell({ entryId, column });
  };

  const handleCellKeyDown = (column: EditorColumn, event: KeyboardEvent<HTMLDivElement>) => {
    // インラインtextareaの文字キーも親セルへbubbleするため、textarea編集中に文字開始処理を再実行しない。
    if (event.target !== event.currentTarget) return;
    if (event.key === "F2" || (column === "target" && event.key === "Enter")) {
      event.preventDefault();
      event.stopPropagation();
      beginEdit(event);
    } else if (
      column === "target" &&
      !event.repeat &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey &&
      event.key.length === 1 &&
      event.key !== " "
    ) {
      event.preventDefault();
      event.stopPropagation();
      beginEdit(event, event.key);
    }
  };

  const selectRow = (event: MouseEvent<HTMLElement>) => {
    const mode = event.shiftKey ? "range" : event.ctrlKey || event.metaKey ? "toggle" : "replace";
    onSelect(entryId, mode);
    const column = event.currentTarget.dataset.column as EditorColumn | undefined;
    if (column) setFocusedCell({ entryId, column });
  };

  const stateClass = `editor-state editor-state--${entry.origin}`;
  // アセット未解決は翻訳を妨げないため、状態列のエラー表示には含めない。
  const warnings = entry.flags
    .filter((flag) => flag !== "asset_missing" && flag !== "asset_ambiguous")
    .map((flag) => FLAG_LABELS[flag]);

  const prepareContextMenu = () => {
    const state = useEditorStore.getState();
    const picked = prepareRowContextSelection(entryId, state.selectedIds, state.selectEntry);
    setContextEntryIds(state.orderedIds.filter((id) => picked.includes(id)));
  };
  const onRowContextMenu = (event: MouseEvent<HTMLDivElement>) => {
    if (!shouldPreserveNativeContextMenu(event.target)) prepareContextMenu();
    contextMenu.triggerProps.onContextMenu(event);
  };
  const onRowKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!((event.shiftKey && event.key === "F10") || event.key === "ContextMenu")) return;
    if (!shouldPreserveNativeContextMenu(event.target)) prepareContextMenu();
    contextMenu.triggerProps.onKeyDown(event);
  };

  const updateDraft = (event: ChangeEvent<HTMLTextAreaElement>) => {
    const input = event.currentTarget;
    const nextDraft = input.value;
    draftRef.current = nextDraft;
    selectionRef.current = [input.selectionStart, input.selectionEnd];
    setDraft(nextDraft);
    if (editorOverlay) requestAnimationFrame(() => resizeExpandedEditor(input));
  };

  const editor = editing ? (
    <textarea
      ref={inputRef}
      aria-label={`${entry.key} の日本語訳`}
      rows={1}
      value={draft}
      onFocus={() => { movingEditorRef.current = false; }}
      onChange={updateDraft}
      onBlur={() => {
        if (!movingEditorRef.current) commit();
      }}
      onKeyDown={(event) => {
        if (isImeCompositionKeyDown(event)) {
          event.stopPropagation();
          return;
        }
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          cancel();
        } else if (event.key === "Enter" && !event.shiftKey) {
          event.preventDefault();
          event.stopPropagation();
          commit(true);
        } else if (event.key === "Tab") {
          if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) {
            // Ctrl+Tab/Shift+Ctrl+TabはModタブ切替へ渡す。下書きは親のflushより先にstoreへ反映する。
            if (event.ctrlKey || event.metaKey) commit();
            return;
          }
          const nextIndex = index + (event.shiftKey ? -1 : 1);
          if (nextIndex >= 0 && nextIndex < useEditorStore.getState().filteredIds.length) {
            event.preventDefault();
            event.stopPropagation();
            commit();
            onMoveToTarget(nextIndex);
          }
        }
      }}
    />
  ) : null;

  // 表のoverflowで長文が切れないよう、拡大時だけ編集欄をbodyへPortalする。
  return (
    <>
    <div
      ref={measureElement}
      className={`editor-table__row${selected ? " editor-table__row--selected" : ""}${aiHighlighted ? " editor-table__row--ai-highlight" : ""}`}
      role="row"
      aria-rowindex={index + 2}
      aria-selected={selected}
      data-entry-id={entryId}
      data-index={index}
      onContextMenu={onRowContextMenu}
      onKeyDown={onRowKeyDown}
      style={{
        "--editor-row-height": `${rowHeight}px`,
        gridTemplateColumns,
        width: tableWidth,
        minHeight: rowHeight,
        transform: `translateY(${rowStart}px)`,
      } as CSSProperties}
    >
      <RowThumbnail entry={entry} projectId={projectId} />
      <div
        className="editor-table__cell editor-table__key"
        role="gridcell"
        aria-colindex={2}
        data-entry-id={entryId}
        data-column="key"
        tabIndex={focusedCell?.entryId === entryId && focusedCell.column === "key" ? 0 : -1}
        onClick={selectRow}
        onFocus={() => focusColumn("key")}
        onKeyDown={(event) => handleCellKeyDown("key", event)}
      >
        {entry.key}
      </div>
      <div
        className="editor-table__cell editor-table__source"
        role="gridcell"
        aria-colindex={3}
        data-entry-id={entryId}
        data-column="source"
        tabIndex={focusedCell?.entryId === entryId && focusedCell.column === "source" ? 0 : -1}
        onClick={selectRow}
        onFocus={() => focusColumn("source")}
        onKeyDown={(event) => handleCellKeyDown("source", event)}
      >
        {entry.sourceText || "—"}
      </div>
      <div
        className={`editor-table__cell editor-table__target${
          focusedCell?.entryId === entryId && focusedCell.column === "target" ? " editor-table__target--focused" : ""
        }`}
        role="gridcell"
        data-entry-id={entryId}
        data-column="target"
        data-testid={`translation-target-${entry.key}`}
        tabIndex={focusedCell?.entryId === entryId && focusedCell.column === "target" ? 0 : -1}
        aria-colindex={4}
        onClick={selectRow}
        onFocus={() => focusColumn("target")}
        onKeyDown={(event) => handleCellKeyDown("target", event)}
        onDoubleClick={beginEdit}
      >
        {editing ? (editorOverlay ? null : editor) : (
          <span className="editor-table__target-text">{entry.targetText || ""}</span>
        )}
      </div>
      <div
        className="editor-table__cell editor-table__state-cell"
        role="gridcell"
        aria-colindex={5}
        data-entry-id={entryId}
        data-column="state"
        tabIndex={focusedCell?.entryId === entryId && focusedCell.column === "state" ? 0 : -1}
        onClick={selectRow}
        onFocus={() => focusColumn("state")}
        onKeyDown={(event) => handleCellKeyDown("state", event)}
      >
        <span className={stateClass} aria-label={ORIGIN_LABELS[entry.origin]}>
          <span aria-hidden="true">{ORIGIN_MARKS[entry.origin]}</span>
          <span>{ORIGIN_LABELS[entry.origin]}</span>
        </span>
        {warnings.length > 0 && (
          <Tooltip content={warnings.join("、")}>
            <span className="editor-table__warning" aria-label={warnings.join("、")}>
              ! <span className="visually-hidden">{warnings.join("、")}</span>
            </span>
          </Tooltip>
        )}
      </div>
      {columns.some((column) => column.id === "namespace") && (
        <div
          className="editor-table__cell editor-table__namespace"
          role="gridcell"
          aria-colindex={6}
          data-entry-id={entryId}
          data-column="namespace"
          tabIndex={focusedCell?.entryId === entryId && focusedCell.column === "namespace" ? 0 : -1}
          onClick={selectRow}
          onFocus={() => focusColumn("namespace")}
          onKeyDown={(event) => handleCellKeyDown("namespace", event)}
        >
          {entry.namespace}
        </div>
      )}
    </div>
    {contextMenu.position && (
      <ContextMenu label="翻訳項目の操作" position={contextMenu.position} onClose={contextMenu.close}>
        {contextEntryIds.length > 1 && <ContextMenuLabel>{contextEntryIds.length.toLocaleString("ja-JP")}件</ContextMenuLabel>}
        <ContextMenuItem
          disabled={aiJobActive}
          aria-keyshortcuts="Control+Enter"
          onClick={() => onTranslateEntries(contextEntryIds)}
        >
          AIで翻訳
          <ContextMenuShortcut>Ctrl+Enter</ContextMenuShortcut>
        </ContextMenuItem>
        <ContextMenuItem
          disabled={aiJobActive}
          onClick={() => {
            const entries = useEditorStore.getState().entries;
            useEditorStore.getState().applyBatchEdits(contextEntryIds.flatMap((id) => {
              const target = entries.get(id);
              return target ? [{ entryId: id, targetText: target.sourceText ?? "" }] : [];
            }));
          }}
        >
          原文を訳にコピー
        </ContextMenuItem>
        <ContextMenuItem
          className="context-menu__item--danger"
          disabled={aiJobActive}
          onClick={() => useEditorStore.getState().applyBatchEdits(contextEntryIds.map((id) => ({ entryId: id, targetText: "" })))}
        >
          訳を削除
        </ContextMenuItem>
        {contextEntryIds.length === 1 && (
          <ContextMenuItem
            disabled={!entry?.sourceText}
            onClick={() => {
              if (entry?.sourceText) onAddGlossary(entry.sourceText, entry.targetText);
            }}
          >
            用語集に追加
          </ContextMenuItem>
        )}
        <ContextMenuSeparator />
        <ContextMenuItem onClick={() => onCopyEntries(contextEntryIds, "key")}>キーをコピー</ContextMenuItem>
        <ContextMenuItem onClick={() => onCopyEntries(contextEntryIds, "source")}>原文をコピー</ContextMenuItem>
        <ContextMenuItem onClick={() => onCopyEntries(contextEntryIds, "target")}>訳をコピー</ContextMenuItem>
      </ContextMenu>
    )}
    {editing && editorOverlay && editor && createPortal(
      <div
        ref={editorOverlayRef}
        className="editor-table__editor-overlay"
        style={{ left: editorOverlay.left, top: editorOverlay.top, width: editorOverlay.width }}
      >
        {entry.sourceText && (
          <div className="editor-table__editor-source">
            <span>原文</span>
            <p>{entry.sourceText}</p>
          </div>
        )}
        {editor}
      </div>,
      document.body,
    )}
    </>
  );
});

function RowThumbnail({ entry, projectId }: { entry: TranslationEntry; projectId: string }) {
  const source = usePreviewThumbnail(entry, projectId);
  return (
    <div className="editor-table__cell editor-table__thumbnail-cell" role="gridcell" aria-colindex={1} aria-label={source ? `${entry.key}のプレビュー` : "プレビューなし"} data-testid={`translation-thumbnail-${entry.key}`}>
      {/* message.* などプレビューと無関係な行に空の枠を並べると表が騒がしくなるため、
          ブロック・アイテム・エンティティのキーの行にだけ枠を出す。 */}
      {isPreviewableEntry(entry) && (
        <span className="editor-table__thumbnail-frame">
          {source && <img src={source} alt="" loading="lazy" />}
        </span>
      )}
    </div>
  );
}

function textareaNeedsExpansion(textarea: HTMLTextAreaElement) {
  return textarea.value.includes("\n") ||
    textarea.scrollWidth > textarea.clientWidth ||
    textarea.scrollHeight > textarea.clientHeight;
}

function estimateTextWidth(text: string, fontSize: number, limit = Number.POSITIVE_INFINITY) {
  let width = 0;
  for (const character of text) {
    const codePoint = character.codePointAt(0) ?? 0;
    const factor = /\s/.test(character) ? 0.35 : codePoint >= 0x2e80 ? 1 : 0.56;
    width += fontSize * factor;
    if (width >= limit) return width;
  }
  return width;
}

function editorOverlayPosition(cell: HTMLElement | null) {
  const bounds = cell?.getBoundingClientRect();
  const scroller = cell?.closest<HTMLElement>(".editor-table__scroll")?.getBoundingClientRect();
  const leftEdge = Math.max(16, (scroller?.left ?? 0) + 8);
  const rightEdge = Math.min(window.innerWidth - 16, scroller?.right ?? window.innerWidth - 16);
  const availableWidth = Math.max(1, rightEdge - leftEdge);
  const width = Math.min(Math.max(bounds?.width ?? 0, 480), availableWidth);
  const anchorTop = Math.max(16, bounds?.top ?? 16);
  return {
    left: Math.max(leftEdge, Math.min(bounds?.left ?? leftEdge, rightEdge - width)),
    top: anchorTop,
    anchorTop,
    width,
  };
}

function resizeExpandedEditor(textarea: HTMLTextAreaElement) {
  textarea.style.height = "auto";
  textarea.style.height = `${Math.min(textarea.scrollHeight, window.innerHeight * 0.65, window.innerHeight * 0.75 - 128)}px`;
}
