import { useDeferredValue, useEffect, useState } from "react";
import type { RefObject } from "react";

import { IconButton, PrimaryButton, TertiaryButton } from "../../components/button/Buttons";
import { Menu, MenuItem, Select, Tooltip } from "../../components/Controls";
import type { RequestConfirmation } from "../../components/useDialogConfirmation";
import type { EditorFilter } from "../../stores/editorStore";
import { useEditorStore } from "../../stores/editorStore";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useProjectStore } from "../../stores/projectStore";
import { formatNumber, useT } from "../../i18n";
import { targetLanguageName } from "../../i18n/minecraftLanguages";
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
  onAddMod?: () => void;
  addDisabled?: boolean;
  editingDisabled?: boolean;
}

export function EditorToolbar({
  searchRef,
  onExport,
  onToggleInspector,
  inspectorOpen,
  onRetrySave,
  saveStatus,
  requestConfirmation,
  onOpenGlossary,
  onAddMod = () => undefined,
  addDisabled = false,
  editingDisabled = false,
}: EditorToolbarProps) {
  const t = useT();
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
  const targetLocale = useProjectStore((state) => state.details?.targetLocale ?? "ja_jp");
  const targetLanguage = targetLanguageName(targetLocale);
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
      ? t("editor.save.saving")
      : saveStatus === "error"
        ? t("editor.save.failed")
        : t("editor.save.saved");
  const reviewCountKey = needsReviewCount === 1 ? "editor.progress.review_one" : "editor.progress.review";

  return (
    <>
      <header className="editor-toolbar">
        <h1 className="editor-toolbar__title">{t("editor.title")}</h1>
        <label className="editor-toolbar__search-label" htmlFor="translation-search">
          <span className="visually-hidden">{t("editor.search.label")}</span>
          <Tooltip content={t("editor.search.description", { language: targetLanguage })} shortcut="Ctrl+F">
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
              placeholder={t("editor.search.label")}
              aria-keyshortcuts="Control+F"
            />
          </Tooltip>
        </label>
        <Menu className="editor-toolbar__filters" aria-label={t("editor.search.filter")} trigger={t("editor.search.filter")} panelRole="group">
          <fieldset className="editor-toolbar__filters-panel">
            <legend className="visually-hidden">{t("editor.search.filterLegend")}</legend>
            <div>
              <span>{t("editor.search.status")}</span>
              <Select
                aria-label={t("editor.search.status")}
                className="editor-toolbar__select"
                options={FILTER_OPTIONS.map(([value, messageKey]) => ({ value, label: t(messageKey) }))}
                value={filter}
                onChange={(value) => setFilter(value as EditorFilter)}
              />
            </div>
            {namespaces.length > 1 && (
              <div>
                <span>{t("editor.column.namespace")}</span>
                <Select
                  aria-label={t("editor.column.namespace")}
                  className="editor-toolbar__select"
                  options={[
                    { value: "all", label: t("editor.filter.all") },
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
          <legend className="visually-hidden">{t("editor.history")}</legend>
          <Tooltip content={t("editor.history.undo")} shortcut="Ctrl+Z">
            <IconButton onClick={undo} disabled={editingDisabled || !canUndo || Boolean(activeAiJob)} aria-label={t("editor.history.undo")} aria-keyshortcuts="Control+Z">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M9 14 4 9l5-5M4 9h8a6 6 0 1 1 0 12" />
              </svg>
            </IconButton>
          </Tooltip>
          <Tooltip content={t("editor.history.redo")} shortcut="Ctrl+Y">
            <IconButton onClick={redo} disabled={editingDisabled || !canRedo || Boolean(activeAiJob)} aria-label={t("editor.history.redo")} aria-keyshortcuts="Control+Y">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="m15 14 5-5-5-5m5 5h-8a6 6 0 1 0 0 12" />
              </svg>
            </IconButton>
          </Tooltip>
        </fieldset>
        <div className="editor-toolbar__progress" aria-live="polite">
          <span>
            {t("editor.progress.completed", { translated: formatNumber(translatedCount), total: formatNumber(orderedCount) })}
          </span>
          <span className="editor-toolbar__review-count">{t(reviewCountKey, { count: formatNumber(needsReviewCount) })}</span>
        </div>
        <div className="editor-toolbar__save">
          <output data-testid="editor-save-status" aria-live="polite">{saveMessage}</output>
          {saveStatus === "error" && (
            <TertiaryButton onClick={onRetrySave}>
              {t("editor.save.retry")}
            </TertiaryButton>
          )}
        </div>
        <Menu className="editor-toolbar__ai-menu" panelAlignment="end" panelClassName="editor-toolbar__ai-menu-panel" aria-label={t("editor.ai.menu")} trigger={t("editor.ai.menu")} panelRole="menu">
          <MenuItem
            disabled={editingDisabled || Boolean(activeAiJob)}
            onClick={() => void translateWithAi(orderedIds.filter((id) => !entries.get(id)?.targetText), requestConfirmation)}
          >
            {t("editor.ai.translateMissing")}
          </MenuItem>
          {modUnitCount > 1 && (
            <MenuItem
              disabled={editingDisabled || Boolean(activeAiJob)}
              onClick={() => void translateWholeProjectWithAi(requestConfirmation)}
            >
              {t("editor.ai.translateProjectMissing")}
            </MenuItem>
          )}
        </Menu>
        <TertiaryButton data-testid="editor-toggle-inspector" onClick={onToggleInspector} aria-expanded={inspectorOpen}>
          {t("editor.inspector.toggle")}
        </TertiaryButton>
        <TertiaryButton data-testid="editor-add-mod" disabled={addDisabled} onClick={onAddMod}>
          {t("editor.add")}
        </TertiaryButton>
        <TertiaryButton onClick={onOpenGlossary}>{t("glossary.title")}</TertiaryButton>
        <Tooltip content={t("editor.export")} shortcut="Ctrl+E">
          <PrimaryButton data-testid="editor-export" onClick={onExport} aria-keyshortcuts="Control+E">
            {t("editor.export")}
          </PrimaryButton>
        </Tooltip>
      </header>
      {activeAiJob && (
        <div className="editor-ai-progress" aria-live="polite">
          <span>{t("editor.ai.progress", { completed: formatNumber(activeAiJob.completed), total: formatNumber(activeAiJob.total) })}</span>
          <TertiaryButton
            disabled={!activeAiJob.id}
            onClick={() => activeAiJob.id && void cancelAiJob(activeAiJob.id)}
            aria-label={t("editor.ai.cancel")}
          >
            {t("editor.ai.cancel")}
          </TertiaryButton>
        </div>
      )}
      {aiNotice && (
        <output className="editor-ai-notice" aria-live="polite">
          <span>{aiNotice}</span>
          <TertiaryButton onClick={clearAiNotice} aria-label={t("editor.notification.close")}>{t("editor.close")}</TertiaryButton>
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
  const t = useT();
  const visible = selectedCount > 0;
  return (
    <div
      className={`editor-selection-toolbar${visible ? " editor-selection-toolbar--visible" : ""}`}
      role="toolbar"
      aria-label={t("editor.selection.ariaLabel")}
      aria-hidden={!visible}
      inert={!visible}
    >
      <span aria-live="polite">{t("editor.selection.count", { count: formatNumber(selectedCount) })}</span>
      <Tooltip content={t("editor.ai.translate")} shortcut="Ctrl+Enter">
        <TertiaryButton
          disabled={disabled || !visible}
          onClick={onTranslate}
          aria-keyshortcuts="Control+Enter"
          tabIndex={visible ? 0 : -1}
        >
          {t("editor.ai.translate")}
        </TertiaryButton>
      </Tooltip>
    </div>
  );
}
