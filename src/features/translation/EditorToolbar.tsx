import { useDeferredValue, useEffect, useState } from "react";
import type { RefObject } from "react";

import { IconButton, PrimaryButton, TertiaryButton } from "../../components/button/Buttons";
import { Menu, MenuItem, Select, Tooltip } from "../../components/Controls";
import type { RequestConfirmation } from "../../components/useDialogConfirmation";
import type { EditorFilter } from "../../stores/editorStore";
import { useEditorStore } from "../../stores/editorStore";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useProjectStore } from "../../stores/projectStore";
import { cancelAiJob } from "../../lib/tauri";
import { FILTER_OPTIONS } from "./TranslationTable";
import type { SaveStatus } from "../../stores/editorStore";
import { translateWholeProjectWithAi, translateWithAi } from "./aiTranslation";

interface EditorToolbarProps {
  searchRef: RefObject<HTMLInputElement | null>;
  onExport: () => void;
  onToggleInspector: () => void;
  inspectorOpen: boolean;
  onRetrySave: () => void;
  saveStatus: SaveStatus;
  requestConfirmation: RequestConfirmation;
  onOpenGlossary: () => void;
  editingDisabled?: boolean;
}

const NUMBER_FORMAT = new Intl.NumberFormat("ja-JP");

export function EditorToolbar({
  searchRef,
  onExport,
  onToggleInspector,
  inspectorOpen,
  onRetrySave,
  saveStatus,
  requestConfirmation,
  onOpenGlossary,
  editingDisabled = false,
}: EditorToolbarProps) {
  const initialQuery = useEditorStore((state) => state.searchQuery);
  const namespaces = useEditorStore((state) => state.namespaces);
  const namespaceFilter = useEditorStore((state) => state.namespaceFilter);
  const filter = useEditorStore((state) => state.filter);
  const orderedCount = useEditorStore((state) => state.orderedIds.length);
  const translatedCount = useEditorStore((state) => state.translatedCount);
  const needsReviewCount = useEditorStore((state) => state.needsReviewCount);
  const orderedIds = useEditorStore((state) => state.orderedIds);
  const entries = useEditorStore((state) => state.entries);
  const activeAiJob = useAiJobStore((state) => state.activeJob);
  const modUnitCount = useProjectStore((state) => state.modUnits.length);
  const aiNotice = useAiJobStore((state) => state.notice);
  const clearAiNotice = useAiJobStore((state) => state.clearNotice);
  const canUndo = useEditorStore((state) => state.undoStack.length > 0);
  const canRedo = useEditorStore((state) => state.redoStack.length > 0);
  const setSearchQuery = useEditorStore((state) => state.setSearchQuery);
  const setFilter = useEditorStore((state) => state.setFilter);
  const setNamespaceFilter = useEditorStore((state) => state.setNamespaceFilter);
  const undo = useEditorStore((state) => state.undo);
  const redo = useEditorStore((state) => state.redo);
  const [query, setQuery] = useState(initialQuery);
  const deferredQuery = useDeferredValue(query);

  useEffect(() => setSearchQuery(deferredQuery), [deferredQuery, setSearchQuery]);

  const saveMessage =
    saveStatus === "saving"
      ? "保存中…"
      : saveStatus === "error"
        ? "保存できませんでした"
        : "保存済み";

  return (
    <>
      <header className="editor-toolbar">
        <h1 className="editor-toolbar__title">
          翻訳
        </h1>
        <label className="editor-toolbar__search-label" htmlFor="translation-search">
          <span className="visually-hidden">検索</span>
          <Tooltip content="キー・原文・日本語を検索" shortcut="Ctrl+F">
            <input
              ref={searchRef}
              id="translation-search"
              className="editor-toolbar__search"
              type="search"
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              value={query}
              onChange={(event) => setQuery(event.currentTarget.value)}
              placeholder="検索"
              aria-keyshortcuts="Control+F"
            />
          </Tooltip>
        </label>
        <Menu className="editor-toolbar__filters" aria-label="絞り込み" trigger="絞り込み" panelRole="group">
          <fieldset className="editor-toolbar__filters-panel">
            <legend className="visually-hidden">翻訳の絞り込み</legend>
            <div>
              <span>状態</span>
              <Select
                aria-label="状態"
                className="editor-toolbar__select"
                options={FILTER_OPTIONS.map(([value, label]) => ({ value, label }))}
                value={filter}
                onChange={(value) => setFilter(value as EditorFilter)}
              />
            </div>
            {namespaces.length > 1 && (
              <div>
                <span>名前空間</span>
                <Select
                  aria-label="名前空間"
                  className="editor-toolbar__select"
                  options={[
                    { value: "all", label: "すべて" },
                    ...namespaces.map((namespace) => ({ value: namespace, label: namespace })),
                  ]}
                  value={namespaceFilter}
                  onChange={setNamespaceFilter}
                />
              </div>
            )}
          </fieldset>
        </Menu>
        <fieldset className="editor-toolbar__history">
          <legend className="visually-hidden">編集履歴</legend>
          <Tooltip content="元に戻す" shortcut="Ctrl+Z">
            <IconButton onClick={undo} disabled={editingDisabled || !canUndo || Boolean(activeAiJob)} aria-label="元に戻す" aria-keyshortcuts="Control+Z">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M9 14 4 9l5-5M4 9h8a6 6 0 1 1 0 12" />
              </svg>
            </IconButton>
          </Tooltip>
          <Tooltip content="やり直す" shortcut="Ctrl+Y">
            <IconButton onClick={redo} disabled={editingDisabled || !canRedo || Boolean(activeAiJob)} aria-label="やり直す" aria-keyshortcuts="Control+Y">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="m15 14 5-5-5-5m5 5h-8a6 6 0 1 0 0 12" />
              </svg>
            </IconButton>
          </Tooltip>
        </fieldset>
        <div className="editor-toolbar__progress" aria-live="polite">
          <span>
            {NUMBER_FORMAT.format(translatedCount)} / {NUMBER_FORMAT.format(orderedCount)} 翻訳済み
          </span>
          <span className="editor-toolbar__review-count">{NUMBER_FORMAT.format(needsReviewCount)}件 要確認</span>
        </div>
        <div className="editor-toolbar__save">
          <output data-testid="editor-save-status" aria-live="polite">{saveMessage}</output>
          {saveStatus === "error" && (
            <TertiaryButton onClick={onRetrySave}>
              再試行
            </TertiaryButton>
          )}
        </div>
        <Menu className="editor-toolbar__ai-menu" panelAlignment="end" panelClassName="editor-toolbar__ai-menu-panel" aria-label="AI翻訳" trigger="AI翻訳" panelRole="menu">
          <MenuItem
            disabled={editingDisabled || Boolean(activeAiJob)}
            onClick={() => void translateWithAi(orderedIds.filter((id) => !entries.get(id)?.targetText), requestConfirmation)}
          >
            未翻訳を翻訳
          </MenuItem>
          {modUnitCount > 1 && (
            <MenuItem
              disabled={editingDisabled || Boolean(activeAiJob)}
              onClick={() => void translateWholeProjectWithAi(requestConfirmation)}
            >
              プロジェクト全体の未翻訳を翻訳
            </MenuItem>
          )}
        </Menu>
        <TertiaryButton data-testid="editor-toggle-inspector" onClick={onToggleInspector} aria-expanded={inspectorOpen}>
          詳細
        </TertiaryButton>
        <TertiaryButton onClick={onOpenGlossary}>用語集</TertiaryButton>
        <Tooltip content="書き出し" shortcut="Ctrl+E">
          <PrimaryButton data-testid="editor-export" onClick={onExport} aria-keyshortcuts="Control+E">
            書き出し
          </PrimaryButton>
        </Tooltip>
      </header>
      {activeAiJob && (
        <div className="editor-ai-progress" aria-live="polite">
          <span>AI翻訳 {activeAiJob.completed.toLocaleString("ja-JP")} / {activeAiJob.total.toLocaleString("ja-JP")}</span>
          <TertiaryButton
            disabled={!activeAiJob.id}
            onClick={() => activeAiJob.id && void cancelAiJob(activeAiJob.id)}
            aria-label="AI翻訳をキャンセル"
          >
            キャンセル
          </TertiaryButton>
        </div>
      )}
      {aiNotice && (
        <output className="editor-ai-notice" aria-live="polite">
          <span>{aiNotice}</span>
          <TertiaryButton onClick={clearAiNotice} aria-label="通知を閉じる">閉じる</TertiaryButton>
        </output>
      )}
    </>
  );
}

export function EditorSelectionToolbar({
  selectedCount,
  disabled,
  onTranslate,
}: {
  selectedCount: number;
  disabled: boolean;
  onTranslate: () => void;
}) {
  const visible = selectedCount > 0;
  return (
    <div
      className={`editor-selection-toolbar${visible ? " editor-selection-toolbar--visible" : ""}`}
      role="toolbar"
      aria-label="選択中の項目"
      aria-hidden={!visible}
      inert={!visible}
    >
      <span aria-live="polite">{NUMBER_FORMAT.format(selectedCount)}件選択中</span>
      <Tooltip content="AIで翻訳" shortcut="Ctrl+Enter">
        <TertiaryButton
          disabled={disabled || !visible}
          onClick={onTranslate}
          aria-keyshortcuts="Control+Enter"
          tabIndex={visible ? 0 : -1}
        >
          AIで翻訳
        </TertiaryButton>
      </Tooltip>
    </div>
  );
}
