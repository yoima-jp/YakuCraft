import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";

import { useAppStore } from "../../stores/appStore";
import { useEditorStore } from "../../stores/editorStore";
import { flushBeforeLeavingEditor } from "../../lib/navigation";
import { AppCommandError, getModUnitRemovalInfo, getProjectSummary, listEntries, listGlossaryTerms, listModUnits, refreshModpackUnit, removeModUnit } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { useProjectStore } from "../../stores/projectStore";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useSettingsStore } from "../../stores/settingsStore";
import { useDialogConfirmation } from "../../components/useDialogConfirmation";
import { isUnitScopedAiJobActive, translateSelectedWithAi, translateWholeProjectWithAi, translateWithAi } from "./aiTranslation";
import { clearModIconCacheInUi, ModTabs, setModIconCacheProject } from "./ModTabs";
import { clearModIconCache } from "./modTabCommands";
import type { ModProgress } from "./ModTabs";
import {
  modTabShortcutIndex,
  readModTabPreferences,
  reorderModTabPreferences,
  visibleModTabUnits,
  writeModTabPreferences,
} from "./modTabState";
import type { ModTabPreferences } from "./modTabState";
import { EditorSelectionToolbar, EditorToolbar } from "./EditorToolbar";
import { Inspector } from "./Inspector";
import { GlossaryPanel } from "./GlossaryPanel";
import type { GlossaryDraft } from "./GlossaryPanel";
import { TauriEditorPersistence } from "./persistence";
import { TranslationTable } from "./TranslationTable";
import {
  minimumResponsiveTableWidth,
  normalizeColumnWidths,
  readUserSizedColumns,
} from "./tableLayout";
import { useAutosave } from "./useAutosave";
import { parseTranslationClipboard, serializeTranslationClipboard } from "./clipboard";
import "./TranslationScreen.css";

/* oxlint-disable jsx-a11y/prefer-tag-over-role -- 幅変更操作を持つseparatorはhrでは表せない。 */

const persistence = new TauriEditorPersistence();

function discardModUnitSnapshot(modUnitId: number) {
  const state = useEditorStore.getState();
  if (!state.unitStates.has(modUnitId)) return;
  const unitStates = new Map(state.unitStates);
  unitStates.delete(modUnitId);
  useEditorStore.setState({ unitStates });
}

function modUnitName(unit: { sourceKind?: "jar" | "modpack"; name: string | null; sourceFilename: string }) {
  return unit.sourceKind === "modpack"
    ? "Modpack 固有（クエスト・KubeJS）"
    : unit.name?.trim() || unit.sourceFilename;
}

export function TranslationScreen() {
  const searchRef = useRef<HTMLInputElement>(null);
  const workspaceRef = useRef<HTMLDivElement>(null);
  const tabSwitchRequestRef = useRef(0);
  const refreshingModpackIdRef = useRef<number | null>(null);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [glossaryOpen, setGlossaryOpen] = useState(false);
  const [glossaryDraft, setGlossaryDraft] = useState<GlossaryDraft | null>(null);
  const [inspectorWidthOverride, setInspectorWidthOverride] = useState<number | null>(null);
  const [sidebarWidthOverride, setSidebarWidthOverride] = useState<number | null>(null);
  const [sidebarCollapsedOverride, setSidebarCollapsedOverride] = useState<boolean | null>(null);
  const [workspaceWidth, setWorkspaceWidth] = useState(0);
  const [wideEditorLayout, setWideEditorLayout] = useState(() => isWideEditorLayout());
  const [loadError, setLoadError] = useState<{ projectId: string; message: string } | null>(null);
  const [modActionError, setModActionError] = useState<string | null>(null);
  const [refreshingModpackId, setRefreshingModpackId] = useState<number | null>(null);
  const [tabOrderAnnouncement, setTabOrderAnnouncement] = useState("");
  const [tabPreferences, setTabPreferences] = useState<ModTabPreferences>({ openIds: [], pinnedIds: [], activeId: 0 });
  const [tabsInitializedProjectId, setTabsInitializedProjectId] = useState<string | null>(null);
  const { requestConfirmation, dialog: confirmationDialog } = useDialogConfirmation();
  const goTo = useAppStore((state) => state.goTo);
  const projectId = useProjectStore((state) => state.details?.project.id ?? null);
  const modUnits = useProjectStore((state) => state.modUnits);
  const activeAiJob = useAiJobStore((state) => state.activeJob);
  const loadedProjectId = useEditorStore((state) => state.loadedProjectId);
  const hasLoadedEntries = useEditorStore((state) => state.hasLoadedEntries);
  const activeModUnitId = useEditorStore((state) => state.activeModUnitId);
  const unitStates = useEditorStore((state) => state.unitStates);
  const hasMultipleNamespaces = useEditorStore((state) => state.namespaces.length > 1);
  const loaded = hasLoadedEntries && loadedProjectId === projectId;
  const selectedIds = useEditorStore((state) => state.selectedIds);
  const focusedCell = useEditorStore((state) => state.focusedCell);
  const filteredIds = useEditorStore((state) => state.filteredIds);
  const loadEntries = useEditorStore((state) => state.loadEntries);
  const translatedCount = useEditorStore((state) => state.translatedCount);
  const needsReviewCount = useEditorStore((state) => state.needsReviewCount);
  const orderedCount = useEditorStore((state) => state.orderedIds.length);
  const saveStatus = useEditorStore((state) => state.saveStatus);
  const { retry } = useAutosave(persistence);
  const editorSettings = useSettingsStore((state) => state.settings);
  const saveSettings = useSettingsStore((state) => state.save);
  const storedInspectorWidth = editorSettings["editor.inspectorWidth"];
  const storedSidebarWidth = editorSettings["editor.modSidebarWidth"];
  const sidebarWidth = sidebarWidthOverride ?? clampSidebarWidth(storedSidebarWidth);
  const sidebarCollapsed = sidebarCollapsedOverride ?? Boolean(editorSettings["editor.modSidebarCollapsed"] ?? false);
  const showVerticalModTabs =
    modUnits.length > 1 &&
    activeModUnitId !== null &&
    editorSettings["editor.modTabPosition"] === "left" &&
    wideEditorLayout;
  const preferredColumnWidths = normalizeColumnWidths(editorSettings["editor.tableColumnWidths"]);
  const userSizedColumns = readUserSizedColumns(editorSettings["editor.tableColumnWidths"]);
  const minimumTableWidth = minimumResponsiveTableWidth(
    hasMultipleNamespaces,
    preferredColumnWidths,
    userSizedColumns,
  );
  const maxInspectorWidth = Math.max(280, Math.min(560, workspaceWidth - minimumTableWidth - 12));
  const inspectorWidth = Math.min(inspectorWidthOverride ?? clampInspectorWidth(storedInspectorWidth), maxInspectorWidth);

  useEffect(() => {
    const updateLayout = () => setWideEditorLayout(isWideEditorLayout());
    window.addEventListener("resize", updateLayout);
    return () => window.removeEventListener("resize", updateLayout);
  }, []);

  useLayoutEffect(() => {
    if (!loaded) return;
    const workspace = workspaceRef.current;
    if (!workspace) return;
    const updateWidth = () => {
      const width = workspace.clientWidth;
      setWorkspaceWidth((current) => current === width ? current : width);
    };
    updateWidth();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", updateWidth);
      return () => window.removeEventListener("resize", updateWidth);
    }
    const observer = new ResizeObserver(updateWidth);
    observer.observe(workspace);
    return () => observer.disconnect();
  }, [loaded]);

  useEffect(() => {
    setModIconCacheProject(projectId);
    void clearModIconCache().catch(() => {});
  }, [projectId]);

  useEffect(() => {
    const setGlossaryTerms = useEditorStore.getState().setGlossaryTerms;
    setGlossaryTerms([]);
    if (!projectId) return;
    let active = true;
    void listGlossaryTerms()
      .then((terms) => {
        if (active) setGlossaryTerms(terms);
      })
      .catch(() => {
        if (active) setGlossaryTerms([]);
      });
    return () => { active = false; };
  }, [projectId]);

  useEffect(() => {
    if (projectId && loadedProjectId === projectId) {
      useAiJobStore.getState().setProject(projectId);
    }
  }, [loadedProjectId, projectId]);

  useEffect(() => {
    if (!projectId) return;
    let active = true;
    void (async () => {
      const units = await listModUnits();
      if (!active) return;
      useProjectStore.getState().setModUnits(units);
      const preferences = readModTabPreferences(projectId, units.map((unit) => unit.id));
      const editor = useEditorStore.getState();
      let currentUnitId = editor.activeModUnitId;

      if (editor.loadedProjectId === projectId && editor.hasLoadedEntries) {
        const firstUnitId = units[0]?.id;
        if (currentUnitId === null && firstUnitId !== undefined) {
          editor.activateLoadedModUnit(firstUnitId, projectId);
          currentUnitId = firstUnitId;
        }
        if (currentUnitId !== preferences.activeId && units.some((unit) => unit.id === preferences.activeId)) {
          const cached = useEditorStore.getState().unitStates.get(preferences.activeId);
          const entries = cached?.projectId === projectId ? [] : await listEntries(preferences.activeId);
          if (!active) return;
          useEditorStore.getState().activateModUnit(preferences.activeId, entries, projectId);
        }
      } else if (units.length > 0) {
        const entries = await listEntries(preferences.activeId);
        if (!active) return;
        loadEntries(entries, projectId);
        useEditorStore.getState().activateLoadedModUnit(preferences.activeId, projectId);
      } else {
        loadEntries([], projectId);
      }

      if (!active) return;
      const current = useEditorStore.getState();
      const currentId = current.activeModUnitId ?? preferences.activeId;
      const normalizedPreferences = {
        ...preferences,
        activeId: currentId,
        openIds: preferences.openIds.includes(currentId)
          ? preferences.openIds
          : [...preferences.openIds, currentId],
      };
      current.discardClosedModUnits([...normalizedPreferences.openIds, ...normalizedPreferences.pinnedIds]);
      setTabPreferences(normalizedPreferences);
      writeModTabPreferences(projectId, normalizedPreferences);
      setTabsInitializedProjectId(projectId);
      setLoadError(null);
    })()
      .catch((error: unknown) => {
        if (!active) return;
        void logFrontendError(error, "load translation data");
        setLoadError({
          projectId,
          message: error instanceof AppCommandError ? error.message : "翻訳データを読み込めませんでした。",
        });
      });
    return () => {
      active = false;
    };
  }, [loadEntries, projectId]);

  const commitTabPreferences = useCallback((next: ModTabPreferences) => {
    if (!projectId) return;
    setTabPreferences(next);
    writeModTabPreferences(projectId, next);
  }, [projectId]);

  const reorderModUnit = useCallback((modUnitId: number, insertionIndex: number, targetPinned: boolean) => {
    const orderedTabs = visibleModTabUnits(modUnits, tabPreferences.openIds, tabPreferences.pinnedIds);
    const next = reorderModTabPreferences(
      tabPreferences,
      orderedTabs.map((unit) => unit.id),
      modUnitId,
      insertionIndex,
      targetPinned,
    );
    if (next === tabPreferences) return false;

    commitTabPreferences(next);
    const movedIndex = visibleModTabUnits(modUnits, next.openIds, next.pinnedIds)
      .findIndex((unit) => unit.id === modUnitId);
    const unit = modUnits.find((candidate) => candidate.id === modUnitId);
    if (movedIndex >= 0 && unit) {
      const name = modUnitName(unit);
      const wasPinned = tabPreferences.pinnedIds.includes(modUnitId);
      const isPinned = next.pinnedIds.includes(modUnitId);
      const pinMessage = wasPinned === isPinned ? ""
        : isPinned ? "、ピン留めしました" : "、ピン留めを解除しました";
      setTabOrderAnnouncement(`${name}を${movedIndex + 1}番目に移動しました${pinMessage}`);
    }
    return true;
  }, [commitTabPreferences, modUnits, tabPreferences]);

  const selectModUnit = useCallback(async (modUnitId: number) => {
    if (isUnitScopedAiJobActive() || refreshingModpackIdRef.current !== null) return;
    if (!projectId || !modUnits.some((unit) => unit.id === modUnitId)) return;
    const requestId = ++tabSwitchRequestRef.current;
    const before = useEditorStore.getState();
    try {
      if (before.activeModUnitId !== modUnitId) {
        if (
          !(await flushBeforeLeavingEditor()) ||
          requestId !== tabSwitchRequestRef.current ||
          refreshingModpackIdRef.current !== null
        ) return;
        const current = useEditorStore.getState();
        if (current.loadedProjectId !== projectId || useProjectStore.getState().details?.project.id !== projectId) return;
        const cached = current.unitStates.get(modUnitId);
        const entries = cached?.projectId === projectId ? [] : await listEntries(modUnitId);
        if (
          requestId !== tabSwitchRequestRef.current ||
          refreshingModpackIdRef.current !== null ||
          useProjectStore.getState().details?.project.id !== projectId ||
          useEditorStore.getState().loadedProjectId !== projectId
        ) return;
        // listEntriesは待機中に旧タブを編集できるため、snapshot化直前にもflushして変更を失わない。
        if (
          !(await flushBeforeLeavingEditor()) ||
          requestId !== tabSwitchRequestRef.current ||
          refreshingModpackIdRef.current !== null
        ) return;
        if (
          useProjectStore.getState().details?.project.id !== projectId ||
          useEditorStore.getState().loadedProjectId !== projectId
        ) return;
        useEditorStore.getState().activateModUnit(modUnitId, entries, projectId);
      }
      if (requestId !== tabSwitchRequestRef.current) return;
      const next = {
        ...tabPreferences,
        openIds: tabPreferences.openIds.includes(modUnitId) ? tabPreferences.openIds : [...tabPreferences.openIds, modUnitId],
        activeId: modUnitId,
      };
      commitTabPreferences(next);
    } catch (error: unknown) {
      if (requestId !== tabSwitchRequestRef.current) return;
      void logFrontendError(error, "switch translation tab");
      setLoadError({
        projectId,
        message: error instanceof AppCommandError ? error.message : "翻訳データを読み込めませんでした。",
      });
    }
  }, [commitTabPreferences, modUnits, projectId, tabPreferences]);

  const closeModUnit = useCallback(async (modUnitId: number) => {
    const visibleIds = new Set([...tabPreferences.openIds, ...tabPreferences.pinnedIds]);
    if (!visibleIds.has(modUnitId) || visibleIds.size <= 1) return;
    let replacementId: number | null = null;
    if (useEditorStore.getState().activeModUnitId === modUnitId) {
      const orderedTabs = visibleModTabUnits(modUnits, tabPreferences.openIds, tabPreferences.pinnedIds);
      const index = orderedTabs.findIndex((unit) => unit.id === modUnitId);
      const replacement = orderedTabs[index - 1] ?? orderedTabs[index + 1];
      if (!replacement) return;
      await selectModUnit(replacement.id);
      if (useEditorStore.getState().activeModUnitId !== replacement.id) return;
      replacementId = replacement.id;
    }
    // ここで使うtabPreferencesはselectModUnit前のclosureなので、切り替え先がピン留めだけのタブだと
    // openIdsに入らない。後でピンを外すと「表示中なのに閉じられない」タブになるため、明示的に足す。
    const remainingOpenIds = tabPreferences.openIds.filter((id) => id !== modUnitId);
    const next = {
      ...tabPreferences,
      openIds:
        replacementId !== null && !remainingOpenIds.includes(replacementId)
          ? [...remainingOpenIds, replacementId]
          : remainingOpenIds,
      pinnedIds: tabPreferences.pinnedIds.filter((id) => id !== modUnitId),
      activeId: useEditorStore.getState().activeModUnitId ?? tabPreferences.activeId,
    };
    commitTabPreferences(next);
    useEditorStore.getState().discardClosedModUnits([...next.openIds, ...next.pinnedIds]);
  }, [commitTabPreferences, modUnits, selectModUnit, tabPreferences]);

  const closeModUnits = useCallback(async (removeIds: number[], fallbackId: number) => {
    const remove = new Set(removeIds);
    if (remove.size === 0) return;
    const currentId = useEditorStore.getState().activeModUnitId;
    let activeId = currentId;
    if (activeId !== null && remove.has(activeId)) {
      await selectModUnit(fallbackId);
      if (useEditorStore.getState().activeModUnitId !== fallbackId) return;
      activeId = fallbackId;
    }
    const openIds = tabPreferences.openIds.filter((id) => !remove.has(id));
    const pinnedIds = tabPreferences.pinnedIds.filter((id) => !remove.has(id));
    if (activeId !== null && !openIds.includes(activeId) && !pinnedIds.includes(activeId)) openIds.push(activeId);
    const next = { openIds, pinnedIds, activeId: activeId ?? fallbackId };
    commitTabPreferences(next);
    useEditorStore.getState().discardClosedModUnits([...next.openIds, ...next.pinnedIds]);
  }, [commitTabPreferences, selectModUnit, tabPreferences]);

  const closeOtherModTabs = useCallback((keepId: number) => {
    const visible = visibleModTabUnits(modUnits, tabPreferences.openIds, tabPreferences.pinnedIds);
    void closeModUnits(visible.filter((unit) => unit.id !== keepId).map((unit) => unit.id), keepId);
  }, [closeModUnits, modUnits, tabPreferences.openIds, tabPreferences.pinnedIds]);

  const closeRightModTabs = useCallback((modUnitId: number) => {
    const visible = visibleModTabUnits(modUnits, tabPreferences.openIds, tabPreferences.pinnedIds);
    const index = visible.findIndex((unit) => unit.id === modUnitId);
    if (index < 0) return;
    void closeModUnits(visible.slice(index + 1).map((unit) => unit.id), modUnitId);
  }, [closeModUnits, modUnits, tabPreferences.openIds, tabPreferences.pinnedIds]);

  const removeModUnitFromProject = useCallback(async (modUnitId: number) => {
    if (!projectId || useAiJobStore.getState().activeJob) return;
    const unit = useProjectStore.getState().modUnits.find((candidate) => candidate.id === modUnitId);
    if (!unit) return;
    const name = modUnitName(unit);
    setModActionError(null);
    try {
      const info = await getModUnitRemovalInfo(modUnitId);
      const warning = [
        info.hasUnrestoredReplacements ? "上書きしたjarを元に戻せなくなります。" : "",
        info.hasUnrestoredFileDeployments ? "配置したファイルを復元できなくなります。" : "",
      ].filter(Boolean).join("\n");
      const confirmed = await requestConfirmation({
        title: `${name} をプロジェクトから外しますか？`,
        message: `${name} をプロジェクトから外します。この mod の訳も削除されます。${warning}`,
        confirmLabel: "プロジェクトから外す",
      });
      if (!confirmed || !(await flushBeforeLeavingEditor())) return;

      const remove = async (confirmUnrestoredReplacements: boolean) => {
        try {
          await removeModUnit(modUnitId, confirmUnrestoredReplacements);
        } catch (error: unknown) {
          void logFrontendError(error, "remove mod from project");
          if (
            !confirmUnrestoredReplacements &&
            error instanceof AppCommandError &&
            error.appError.code === "unit_deployments_pending"
          ) {
            const accepted = await requestConfirmation({
              title: `${name} をプロジェクトから外しますか？`,
              message: `${name} をプロジェクトから外します。この mod の訳も削除されます。\n配置したファイルのバックアップを復元できなくなります。`,
              confirmLabel: "プロジェクトから外す",
            });
            if (accepted) await remove(true);
            return;
          }
          throw error;
        }
      };
      await remove(info.hasUnrestoredReplacements);
      // A pending-backup retry can be declined; keep the current store until a removal actually succeeds.
      const nextUnits = await listModUnits();
      if (nextUnits.length === useProjectStore.getState().modUnits.length) return;
      const nextDetails = await getProjectSummary();
      useProjectStore.getState().setProjectDetails(nextDetails);
      useProjectStore.getState().setModUnits(nextUnits);
      clearModIconCacheInUi();

      const remainingIds = nextUnits.map((candidate) => candidate.id);
      const currentActiveId = useEditorStore.getState().activeModUnitId;
      const activeWasRemoved = currentActiveId === modUnitId;
      const nextActiveId = activeWasRemoved ? nextUnits[0]?.id ?? null : currentActiveId;
      if (activeWasRemoved && nextActiveId !== null) {
        await selectModUnit(nextActiveId);
        if (useEditorStore.getState().activeModUnitId !== nextActiveId) return;
      } else if (activeWasRemoved) {
        useEditorStore.getState().loadEntries([], projectId);
      }
      const openIds = tabPreferences.openIds.filter((id) => id !== modUnitId && remainingIds.includes(id));
      const pinnedIds = tabPreferences.pinnedIds.filter((id) => id !== modUnitId && remainingIds.includes(id));
      if (nextActiveId !== null && !openIds.includes(nextActiveId) && !pinnedIds.includes(nextActiveId)) openIds.push(nextActiveId);
      const next = { openIds, pinnedIds, activeId: nextActiveId ?? 0 };
      commitTabPreferences(next);
      useEditorStore.getState().discardClosedModUnits(remainingIds);
    } catch (error: unknown) {
      void logFrontendError(error, "remove mod from project");
      setModActionError(error instanceof AppCommandError ? error.message : "modをプロジェクトから外せませんでした。もう一度お試しください。");
    }
  }, [commitTabPreferences, projectId, requestConfirmation, selectModUnit, tabPreferences]);

  const refreshModpackFromInstance = useCallback(async (modUnitId: number) => {
    if (!projectId || useAiJobStore.getState().activeJob || refreshingModpackIdRef.current !== null) return;
    const unit = useProjectStore.getState().modUnits.find((candidate) => candidate.id === modUnitId);
    if (unit?.sourceKind !== "modpack" || useEditorStore.getState().activeModUnitId !== modUnitId) return;
    refreshingModpackIdRef.current = modUnitId;
    tabSwitchRequestRef.current += 1;
    setRefreshingModpackId(modUnitId);
    setModActionError(null);
    try {
      if (!(await flushBeforeLeavingEditor())) return;
      await refreshModpackUnit(modUnitId);
      if (useProjectStore.getState().details?.project.id !== projectId) return;
      const [entries, units, details] = await Promise.all([
        listEntries(modUnitId),
        listModUnits(),
        getProjectSummary(),
      ]);
      if (useProjectStore.getState().details?.project.id !== projectId) return;
      useProjectStore.getState().setModUnits(units);
      useProjectStore.getState().setProjectDetails(details);
      const editorAfterReload = useEditorStore.getState();
      if (
        editorAfterReload.loadedProjectId === projectId &&
        editorAfterReload.activeModUnitId === modUnitId
      ) {
        editorAfterReload.loadEntries(entries, projectId);
        useEditorStore.getState().activateLoadedModUnit(modUnitId, projectId);
      } else if (editorAfterReload.loadedProjectId === projectId) {
        discardModUnitSnapshot(modUnitId);
      }
      clearModIconCacheInUi();
    } catch (error: unknown) {
      void logFrontendError(error, "refresh modpack");
      if (useProjectStore.getState().details?.project.id === projectId) {
        setModActionError(error instanceof AppCommandError ? error.message : "Modpack を再取り込みできませんでした。");
      }
    } finally {
      if (refreshingModpackIdRef.current === modUnitId) refreshingModpackIdRef.current = null;
      setRefreshingModpackId(null);
    }
  }, [projectId]);

  const togglePinnedModUnit = useCallback((modUnitId: number) => {
    const wasPinned = tabPreferences.pinnedIds.includes(modUnitId);
    const next = {
      ...tabPreferences,
      pinnedIds: wasPinned
        ? tabPreferences.pinnedIds.filter((id) => id !== modUnitId)
        : [...tabPreferences.pinnedIds, modUnitId],
    };
    commitTabPreferences(next);
    if (wasPinned && !next.openIds.includes(modUnitId)) {
      useEditorStore.getState().discardClosedModUnits([...next.openIds, ...next.pinnedIds]);
    }
  }, [commitTabPreferences, tabPreferences]);

  const progressById = useMemo(() => {
    const progress = new Map<number, ModProgress>();
    for (const unit of modUnits) {
      if (unit.id === activeModUnitId && loaded) {
        progress.set(unit.id, { translated: translatedCount, total: orderedCount, needsReview: needsReviewCount });
      } else {
        const cached = unitStates.get(unit.id);
        progress.set(unit.id, cached?.projectId === projectId
          ? { translated: cached.translatedCount, total: cached.orderedIds.length, needsReview: cached.needsReviewCount }
          : { translated: unit.translated, total: unit.total, needsReview: unit.needsReview });
      }
    }
    return progress;
  }, [activeModUnitId, loaded, modUnits, needsReviewCount, orderedCount, projectId, translatedCount, unitStates]);

  useEffect(() => {
    const handleTabReorderShortcut = (event: KeyboardEvent) => {
      if (
        useAppStore.getState().route !== "editor" ||
        isUnitScopedAiJobActive() ||
        refreshingModpackIdRef.current !== null
      ) return;
      if (!(event.ctrlKey || event.metaKey) || !event.shiftKey || event.altKey) return;
      if (event.key !== "PageUp" && event.key !== "PageDown") return;
      if (document.querySelector('[aria-modal="true"]')) return;

      // Capture before the grid's target handler so PageUp/PageDown only reorders tabs.
      event.preventDefault();
      event.stopPropagation();
      const visible = visibleModTabUnits(modUnits, tabPreferences.openIds, tabPreferences.pinnedIds);
      const activeIndex = visible.findIndex((unit) => unit.id === useEditorStore.getState().activeModUnitId);
      const direction = event.key === "PageUp" ? -1 : 1;
      const active = visible[activeIndex];
      const neighbor = visible[activeIndex + direction];
      if (active && neighbor) {
        reorderModUnit(
          active.id,
          activeIndex + direction,
          tabPreferences.pinnedIds.includes(neighbor.id),
        );
      }
    };
    window.addEventListener("keydown", handleTabReorderShortcut, true);
    return () => window.removeEventListener("keydown", handleTabReorderShortcut, true);
  }, [modUnits, reorderModUnit, tabPreferences.openIds, tabPreferences.pinnedIds]);

  useEffect(() => {
    const handleTabShortcut = (event: KeyboardEvent) => {
      if (useAppStore.getState().route !== "editor" || isUnitScopedAiJobActive()) return;
      if (!(event.ctrlKey || event.metaKey)) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      if (event.key !== "Tab" && event.altKey) return;
      const visible = visibleModTabUnits(modUnits, tabPreferences.openIds, tabPreferences.pinnedIds);
      const activeIndex = visible.findIndex((unit) => unit.id === useEditorStore.getState().activeModUnitId);
      const nextIndex = modTabShortcutIndex(event.key, visible.length, activeIndex, event.shiftKey);
      if (nextIndex === null) return;
      const unit = visible[nextIndex];
      if (!unit) return;
      event.preventDefault();
      void selectModUnit(unit.id);
    };
    window.addEventListener("keydown", handleTabShortcut);
    return () => window.removeEventListener("keydown", handleTabShortcut);
  }, [modUnits, selectModUnit, tabPreferences.openIds, tabPreferences.pinnedIds]);

  const copyEntries = useCallback(async (ids: number[], column: "key" | "source" | "target") => {
    const state = useEditorStore.getState();
    const value = serializeTranslationClipboard(ids.map((id) => {
      const entry = state.entries.get(id);
      if (!entry) return "";
      if (column === "key") return entry.key;
      if (column === "source") return entry.sourceText ?? "";
      return entry.targetText;
    }));
    if (ids.length === 0) return;
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // Clipboard権限はホスト側で拒否される場合があるため、翻訳データには触れない。
    }
  }, []);

  const copySelection = useCallback(async () => {
    const state = useEditorStore.getState();
    const picked = state.selectedIds.length > 0 ? new Set(state.selectedIds) : null;
    const ids = picked
      ? state.orderedIds.filter((id) => picked.has(id))
      : state.focusedCell
        ? [state.focusedCell.entryId]
        : [];
    await copyEntries(ids, "target");
  }, [copyEntries]);

  const pasteClipboard = useCallback(async () => {
    try {
      const value = await navigator.clipboard.readText();
      if (value.length === 0) return;
      const targetTexts = parseTranslationClipboard(value);
      const state = useEditorStore.getState();
      const selected = new Set(state.selectedIds);
      const firstVisibleSelected = state.filteredIds.find((id) => selected.has(id));
      const startId = firstVisibleSelected ?? state.focusedCell?.entryId ?? state.filteredIds[0];
      const startIndex = startId === undefined ? -1 : state.filteredIds.indexOf(startId);
      if (startIndex < 0 || targetTexts.length === 0) return;
      const remaining = state.filteredIds.length - startIndex;
      if (
        targetTexts.length > remaining &&
        !(await requestConfirmation({
          title: "一部だけ貼り付けますか？",
          message: `表示範囲に収まらないため、表示中の${remaining}件に貼り付けます。`,
          confirmLabel: "貼り付け",
        }))
      ) {
        return;
      }
      useEditorStore.getState().pasteTranslations(targetTexts, startId);
    } catch {
      // Clipboard権限が拒否された場合は、貼り付け処理だけを中止する。
    }
  }, [requestConfirmation]);

  useEffect(() => {
    const handleShortcut = (event: KeyboardEvent) => {
      // Settings表示中も画面を保持するため、背面のEditorがキー操作を処理しないようにする。
      if (useAppStore.getState().route !== "editor") return;
      // 確認Dialog中は背面の編集ショートカットを止め、確認対象の操作が入れ替わらないようにする。
      if (document.querySelector('[aria-modal="true"]')) return;
      if (refreshingModpackIdRef.current !== null) return;
      if (
        event.key === "Escape" &&
        !(event.target instanceof HTMLElement && event.target.matches("input, textarea, select"))
      ) {
        const state = useEditorStore.getState();
        if (state.selectedIds.length > 0) state.clearSelection();
        else if (inspectorOpen) setInspectorOpen(false);
        return;
      }
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key === "enter") {
        const selectedState = useEditorStore.getState();
        if (selectedState.selectedIds.length > 0) {
          event.preventDefault();
          void translateSelectedWithAi(requestConfirmation);
        }
      } else if (key === "f") {
        event.preventDefault();
        searchRef.current?.focus();
      } else if ((key === "z" || key === "y") && event.target instanceof HTMLElement && event.target.matches("input, textarea")) {
        return;
      } else if (key === "z") {
        event.preventDefault();
        if (useAiJobStore.getState().activeJob) return;
        useEditorStore.getState().undo();
      } else if (key === "y") {
        event.preventDefault();
        if (useAiJobStore.getState().activeJob) return;
        useEditorStore.getState().redo();
      } else if (key === "e") {
        event.preventDefault();
        goTo("export");
      }
    };

    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, [goTo, inspectorOpen, requestConfirmation]);

  const entryId = focusedCell?.entryId ?? selectedIds[0] ?? filteredIds[0] ?? null;
  const modTabsProps = {
    projectId: projectId!,
    units: modUnits,
    openIds: tabPreferences.openIds,
    pinnedIds: tabPreferences.pinnedIds,
    activeUnitId: activeModUnitId!,
    selectionDisabled:
      refreshingModpackId !== null || (Boolean(activeAiJob) && isUnitScopedAiJobActive()),
    actionsDisabled: Boolean(activeAiJob) || refreshingModpackId !== null,
    progressById,
    onSelect: (modUnitId: number) => void selectModUnit(modUnitId),
    onClose: (modUnitId: number) => void closeModUnit(modUnitId),
    onCloseOthers: closeOtherModTabs,
    onCloseRight: closeRightModTabs,
    onTogglePin: togglePinnedModUnit,
    onReorder: reorderModUnit,
    onTranslateMissing: (modUnitId: number) => void translateWholeProjectWithAi(requestConfirmation, modUnitId),
    onRefreshModpack: (modUnitId: number) => void refreshModpackFromInstance(modUnitId),
    onRemove: (modUnitId: number) => void removeModUnitFromProject(modUnitId),
  };

  const currentLoadError = loadError?.projectId === projectId ? loadError.message : null;
  if (currentLoadError) {
    return <section className="editor-screen editor-screen--message" role="alert">{currentLoadError}</section>;
  }

  if (!loaded || !projectId || tabsInitializedProjectId !== projectId) {
    return <section className="editor-screen editor-screen--message"><output aria-live="polite">読み込み中…</output></section>;
  }

  return (
    <section
      className="editor-screen"
      aria-label="翻訳エディター"
      data-wrap-source={Boolean(editorSettings["editor.wrapSource"])}
      data-wrap-target={Boolean(editorSettings["editor.wrapTarget"])}
      style={{ "--editor-font-size": `${Number(editorSettings["editor.fontSize"] ?? 13)}px` } as CSSProperties}
    >
      <EditorToolbar
        key={activeModUnitId ?? "single"}
        searchRef={searchRef}
        onExport={() => goTo("export")}
        onToggleInspector={() => setInspectorOpen((open) => !open)}
        inspectorOpen={inspectorOpen}
        onRetrySave={retry}
        saveStatus={saveStatus}
        editingDisabled={refreshingModpackId !== null}
        requestConfirmation={requestConfirmation}
        onOpenGlossary={() => {
          setGlossaryDraft(null);
          setGlossaryOpen(true);
        }}
      />
      <span className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">{tabOrderAnnouncement}</span>
      {modActionError && <p className="status-message" role="alert">{modActionError}</p>}
      {refreshingModpackId !== null && (
        <p className="status-message" role="status">Modpack を再取り込み中…</p>
      )}
      <div
        ref={workspaceRef}
        className={`editor-workspace${inspectorOpen ? "" : " editor-workspace--inspector-closed"}`}
        inert={refreshingModpackId !== null}
        style={{
          "--inspector-width": `${inspectorWidth}px`,
          "--minimum-table-width": `${minimumTableWidth}px`,
        } as CSSProperties}
      >
        <div className={`editor-workspace__table${showVerticalModTabs ? " editor-workspace__table--vertical-tabs" : ""}`}>
          {showVerticalModTabs && (
            <ModTabs
              key={projectId}
              {...modTabsProps}
              orientation="vertical"
              sidebarWidth={sidebarWidth}
              sidebarCollapsed={sidebarCollapsed}
              onSidebarWidthChange={setSidebarWidthOverride}
              onSidebarWidthCommit={(width) => void saveSettings({ "editor.modSidebarWidth": width }).catch(() => undefined)}
              onToggleSidebarCollapsed={() => {
                const collapsed = !sidebarCollapsed;
                setSidebarCollapsedOverride(collapsed);
                void saveSettings({ "editor.modSidebarCollapsed": collapsed }).catch(() => undefined);
              }}
            />
          )}
          <div className="editor-workspace__table-main">
            {!showVerticalModTabs && modUnits.length > 1 && activeModUnitId !== null && (
              <ModTabs key={projectId} {...modTabsProps} orientation="horizontal" />
            )}
            <div
              className="editor-workspace__table-panel"
              {...(modUnits.length > 1 && activeModUnitId !== null
                ? {
                    role: "tabpanel",
                    id: "translation-table-panel",
                    ...(showVerticalModTabs
                      ? { "aria-label": "翻訳一覧" }
                      : { "aria-labelledby": `mod-tab-${activeModUnitId}` }),
                  }
                : {})}
            >
              <TranslationTable
                onCopy={() => void copySelection()}
                onCopyEntries={(ids, column) => void copyEntries(ids, column)}
                onTranslateEntries={(ids) => void translateWithAi(ids, requestConfirmation)}
                onAddGlossary={(sourceTerm, targetTerm) => {
                  setGlossaryDraft({ sourceTerm, targetTerm });
                  setGlossaryOpen(true);
                }}
                aiJobActive={Boolean(activeAiJob)}
                onPaste={() => void pasteClipboard()}
              />
              <EditorSelectionToolbar
                selectedCount={selectedIds.length}
                disabled={Boolean(activeAiJob)}
                onTranslate={() => void translateSelectedWithAi(requestConfirmation)}
              />
            </div>
          </div>
        </div>
        {inspectorOpen && (
          <InspectorResizeHandle
            width={inspectorWidth}
            maxWidth={maxInspectorWidth}
            onResize={setInspectorWidthOverride}
            onCommit={(width) => void saveSettings({ "editor.inspectorWidth": width }).catch(() => undefined)}
          />
        )}
        <Inspector
          entryId={entryId}
          open={inspectorOpen}
          onClose={() => setInspectorOpen(false)}
          onTranslate={(id) => void translateSelectedWithAi(requestConfirmation, id)}
        />
      </div>
      <GlossaryPanel
        open={glossaryOpen}
        initialDraft={glossaryDraft}
        onClose={() => {
          setGlossaryOpen(false);
          setGlossaryDraft(null);
        }}
      />
      {confirmationDialog}
    </section>
  );
}

function clampInspectorWidth(value: unknown) {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(560, Math.max(280, value))
    : 320;
}

function clampSidebarWidth(value: unknown) {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(420, Math.max(200, value))
    : 256;
}

function isWideEditorLayout() {
  if (typeof window === "undefined") return false;
  return typeof window.matchMedia === "function"
    ? window.matchMedia("(min-width: 68.75rem)").matches
    : window.innerWidth >= 1100;
}

function InspectorResizeHandle({
  width,
  maxWidth,
  onResize,
  onCommit,
}: {
  width: number;
  maxWidth: number;
  onResize: (width: number) => void;
  onCommit: (width: number) => void;
}) {
  const dragStart = useRef<{ pointerId: number; x: number; width: number; currentWidth: number } | null>(null);
  const resizeFromBoundary = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragStart.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    const nextWidth = Math.min(maxWidth, Math.max(280, drag.width + drag.x - event.clientX));
    drag.currentWidth = nextWidth;
    onResize(nextWidth);
  }, [maxWidth, onResize]);

  const stopResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragStart.current;
    if (drag?.pointerId !== event.pointerId) return;
    dragStart.current = null;
    onCommit(drag.currentWidth);
  };

  return (
    <div
      className="editor-workspace__resize-handle"
      role="separator"
      aria-label="表と詳細の幅"
      aria-orientation="vertical"
      aria-valuemin={280}
      aria-valuemax={maxWidth}
      aria-valuenow={width}
      aria-valuetext={`${width} ピクセル`}
      aria-keyshortcuts="Alt+ArrowLeft Alt+ArrowRight"
      tabIndex={0}
      onPointerDown={(event) => {
        event.preventDefault();
        dragStart.current = { pointerId: event.pointerId, x: event.clientX, width, currentWidth: width };
        event.currentTarget.setPointerCapture?.(event.pointerId);
      }}
      onPointerMove={resizeFromBoundary}
      onPointerUp={stopResize}
      onPointerCancel={stopResize}
      onKeyDown={(event) => {
        if (!event.altKey || (event.key !== "ArrowLeft" && event.key !== "ArrowRight")) return;
        event.preventDefault();
        const direction = event.key === "ArrowLeft" ? 1 : -1;
        const nextWidth = Math.min(maxWidth, Math.max(280, width + direction * 16));
        onResize(nextWidth);
        onCommit(nextWidth);
      }}
    />
  );
}
