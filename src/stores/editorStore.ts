import { create } from "zustand";

import type { EntryFlag, GlossaryTerm, TranslationEntry, TranslationOrigin } from "../types/translation";
import { flagsForTarget, mockPlaceholderValidator } from "../features/translation/validation";
import type { PlaceholderValidator } from "../features/translation/validation";
import { GlossaryMatcher } from "../features/translation/glossaryMatcher";

export type EditorFilter =
  | "all"
  | "missing"
  | "existing"
  | "ai"
  | "edited"
  | "needs_review"
  | "source_fallback"
  | "placeholder_error"
  | "asset_missing"
  | "glossary_mismatch";

export type EditorColumn = "key" | "source" | "target" | "state" | "namespace";

export interface FocusedCell {
  entryId: number;
  column: EditorColumn;
}

function focusWithinResults(
  focusedCell: FocusedCell | null,
  filteredIds: number[],
): FocusedCell | null {
  if (focusedCell && filteredIds.includes(focusedCell.entryId)) return focusedCell;
  const firstId = filteredIds[0];
  return firstId === undefined ? null : { entryId: firstId, column: focusedCell?.column ?? "target" };
}

export interface EntryEdit {
  entryId: number;
  before: string;
  after: string;
  beforeOrigin: TranslationOrigin;
  afterOrigin: TranslationOrigin;
  beforeFlags: EntryFlag[];
  afterFlags: EntryFlag[];
  timestamp: number;
  mergeable?: boolean;
}

export type SaveStatus = "saved" | "saving" | "error";

export interface EditorUnitSnapshot {
  projectId: string;
  modUnitId: number;
  entries: Map<number, TranslationEntry>;
  orderedIds: number[];
  filteredIds: number[];
  namespaces: string[];
  selectedIds: number[];
  selectedIdSet: Set<number>;
  selectionAnchorId: number | null;
  focusedCell: FocusedCell | null;
  aiHighlightedIds: Set<number>;
  searchQuery: string;
  filter: EditorFilter;
  namespaceFilter: string;
  dirtyIds: number[];
  dirtyRevision: number;
  undoStack: EntryEdit[][];
  redoStack: EntryEdit[][];
  translatedCount: number;
  needsReviewCount: number;
  scrollTop: number;
}

interface DeferredModAiUndoGroup {
  projectId: string;
  changes: EntryEdit[];
  historyBoundary: EntryEdit[][];
}

interface TargetEdit {
  entryId: number;
  targetText: string;
}

interface EditorStoreState {
  aiHistoryPending: boolean;
  entries: Map<number, TranslationEntry>;
  glossaryTerms: GlossaryTerm[];
  glossaryMatcher: GlossaryMatcher | null;
  orderedIds: number[];
  hasLoadedEntries: boolean;
  loadedProjectId: string | null;
  activeModUnitId: number | null;
  unitStates: Map<number, EditorUnitSnapshot>;
  deferredModAiUndoGroups: Map<number, DeferredModAiUndoGroup[]>;
  scrollTop: number;
  filteredIds: number[];
  namespaces: string[];
  selectedIds: number[];
  selectedIdSet: Set<number>;
  selectionAnchorId: number | null;
  focusedCell: FocusedCell | null;
  aiHighlightedIds: Set<number>;
  searchQuery: string;
  filter: EditorFilter;
  namespaceFilter: string;
  dirtyIds: number[];
  dirtyRevision: number;
  undoStack: EntryEdit[][];
  redoStack: EntryEdit[][];
  translatedCount: number;
  needsReviewCount: number;
  saveStatus: SaveStatus;
  saveError: string | null;
  setGlossaryTerms: (terms: GlossaryTerm[]) => void;
  loadEntries: (entries: TranslationEntry[], projectId?: string) => void;
  activateLoadedModUnit: (modUnitId: number, projectId: string) => void;
  activateModUnit: (modUnitId: number, entries: TranslationEntry[], projectId: string) => void;
  discardClosedModUnits: (keepIds: number[]) => void;
  setScrollTop: (scrollTop: number) => void;
  mergeExternalModUnitEntries: (modUnitId: number, entries: TranslationEntry[], highlightedIds?: number[]) => void;
  finishExternalModUnitAiUndoGroup: (
    modUnitId: number,
    changes: EntryEdit[],
    historyBoundary: EntryEdit[][],
    projectId: string,
  ) => void;
  refreshCachedModUnitEntries: (modUnitId: number, entries: TranslationEntry[], baseline?: Map<number, TranslationEntry>) => void;
  setSearchQuery: (query: string) => void;
  setFilter: (filter: EditorFilter) => void;
  setNamespaceFilter: (namespace: string) => void;
  selectEntry: (entryId: number, mode?: "replace" | "toggle" | "range") => void;
  clearSelection: () => void;
  setFocusedCell: (cell: FocusedCell | null) => void;
  editEntry: (entryId: number, targetText: string, timestamp?: number, validator?: PlaceholderValidator) => void;
  applyBatchEdits: (edits: TargetEdit[], timestamp?: number, validator?: PlaceholderValidator) => void;
  pasteTranslations: (targetTexts: string[], startId?: number) => void;
  applyAiTranslation: (entryId: number, targetText: string, timestamp?: number, validator?: PlaceholderValidator) => void;
  applyAiTranslations: (edits: TargetEdit[], timestamp?: number, validator?: PlaceholderValidator) => void;
  applyExternalAiBatch: (
    entries: Map<number, TranslationEntry>,
    entryIds: number[],
    highlightedIds?: number[],
  ) => void;
  finishAiUndoGroup: (changes: EntryEdit[], historyBoundary?: EntryEdit[][]) => void;
  undo: () => void;
  redo: () => void;
  getDirtyEntries: () => TranslationEntry[];
  markSaved: (savedEntries: TranslationEntry[]) => void;
  markSaveError: () => void;
  setSaveStatus: (status: SaveStatus, error?: string | null) => void;
}

// 確定操作が500ms以内に続いた場合だけ同じUndo単位にし、貼り付けは別途1グループにする。
const MERGE_WINDOW_MS = 500;
const aiHighlightTimers = new Map<number, ReturnType<typeof setTimeout>>();

function isTranslated(entry: TranslationEntry) {
  return entry.targetText.length > 0;
}

function matchesFilter(entry: TranslationEntry, filter: EditorFilter, glossaryMatcher: GlossaryMatcher | null) {
  switch (filter) {
    case "all":
      return true;
    case "missing":
      return entry.targetText.length === 0;
    case "existing":
    case "ai":
    case "edited":
      return entry.origin === filter;
    case "needs_review":
    case "source_fallback":
    case "placeholder_error":
    case "asset_missing":
      return entry.flags.includes(filter);
    case "glossary_mismatch":
      return (glossaryMatcher?.mismatches(entry.sourceText, entry.targetText).length ?? 0) > 0;
  }
}

function buildFilteredIds(state: Pick<
  EditorStoreState,
  "entries" | "orderedIds" | "searchQuery" | "filter" | "namespaceFilter" | "glossaryMatcher"
>) {
  const query = state.searchQuery.toLowerCase();
  const result: number[] = [];

  for (const id of state.orderedIds) {
    const entry = state.entries.get(id);
    if (!entry || !matchesFilter(entry, state.filter, state.glossaryMatcher)) continue;
    if (state.namespaceFilter !== "all" && entry.namespace !== state.namespaceFilter) continue;
    if (
      query &&
      !entry.key.toLowerCase().includes(query) &&
      !(entry.sourceText ?? "").toLowerCase().includes(query) &&
      !entry.targetText.toLowerCase().includes(query)
    ) {
      continue;
    }
    result.push(id);
  }
  return result;
}

function uniqueDirtyIds(previous: number[], changes: EntryEdit[]) {
  const next = new Set(previous);
  for (const change of changes) next.add(change.entryId);
  return [...next];
}

function applyChangeGroup(
  entries: Map<number, TranslationEntry>,
  changes: EntryEdit[],
  direction: "undo" | "redo",
) {
  const next = new Map(entries);
  for (const change of changes) {
    const entry = next.get(change.entryId);
    if (!entry) continue;
    const undoing = direction === "undo";
    next.set(change.entryId, {
      ...entry,
      targetText: undoing ? change.before : change.after,
      origin: undoing ? change.beforeOrigin : change.afterOrigin,
      flags: [...(undoing ? change.beforeFlags : change.afterFlags)],
    });
  }
  return next;
}

function insertHistoryGroup(
  undoStack: EntryEdit[][],
  changes: EntryEdit[],
  historyBoundary: EntryEdit[][],
) {
  const next = [...undoStack];
  const precedingGroup = [...historyBoundary].reverse().find((group) => next.includes(group));
  const insertionIndex = precedingGroup ? next.indexOf(precedingGroup) + 1 : 0;
  next.splice(insertionIndex, 0, changes);
  return next;
}

function countsFor(entries: Map<number, TranslationEntry>) {
  let translatedCount = 0;
  let needsReviewCount = 0;
  for (const entry of entries.values()) {
    if (isTranslated(entry)) translatedCount += 1;
    if (entry.flags.includes("needs_review")) needsReviewCount += 1;
  }
  return { translatedCount, needsReviewCount };
}

function createUnitSnapshot(
  modUnitId: number,
  entries: TranslationEntry[],
  projectId: string,
): EditorUnitSnapshot {
  const entryMap = new Map(entries.map((entry) => [entry.id, entry]));
  const orderedIds = entries.map((entry) => entry.id);
  const namespaces = [...new Set(entries.map((entry) => entry.namespace))].sort();
  const counts = countsFor(entryMap);
  const initial = {
    entries: entryMap,
    orderedIds,
    searchQuery: "",
    filter: "all" as EditorFilter,
    namespaceFilter: "all",
    glossaryMatcher: null,
  };
  const filteredIds = buildFilteredIds({ ...initial, namespaceFilter: "all" });
  return {
    projectId,
    modUnitId,
    ...initial,
    filteredIds,
    namespaces,
    selectedIds: [],
    selectedIdSet: new Set(),
    selectionAnchorId: null,
    focusedCell: focusWithinResults(null, filteredIds),
    aiHighlightedIds: new Set(),
    dirtyIds: [],
    dirtyRevision: 0,
    undoStack: [],
    redoStack: [],
    ...counts,
    scrollTop: 0,
  };
}

function snapshotUnitState(
  state: EditorStoreState,
  modUnitId: number,
  projectId: string,
): EditorUnitSnapshot {
  return {
    projectId,
    modUnitId,
    entries: state.entries,
    orderedIds: state.orderedIds,
    filteredIds: state.filteredIds,
    namespaces: state.namespaces,
    selectedIds: state.selectedIds,
    selectedIdSet: state.selectedIdSet,
    selectionAnchorId: state.selectionAnchorId,
    focusedCell: state.focusedCell,
    aiHighlightedIds: state.aiHighlightedIds,
    searchQuery: state.searchQuery,
    filter: state.filter,
    namespaceFilter: state.namespaceFilter,
    dirtyIds: state.dirtyIds,
    dirtyRevision: state.dirtyRevision,
    undoStack: state.undoStack,
    redoStack: state.redoStack,
    translatedCount: state.translatedCount,
    needsReviewCount: state.needsReviewCount,
    scrollTop: state.scrollTop,
  };
}

function unitSnapshotFields(snapshot: EditorUnitSnapshot): Partial<EditorStoreState> {
  return {
    entries: snapshot.entries,
    orderedIds: snapshot.orderedIds,
    filteredIds: snapshot.filteredIds,
    namespaces: snapshot.namespaces,
    selectedIds: snapshot.selectedIds,
    selectedIdSet: snapshot.selectedIdSet,
    selectionAnchorId: snapshot.selectionAnchorId,
    focusedCell: snapshot.focusedCell,
    aiHighlightedIds: snapshot.aiHighlightedIds,
    searchQuery: snapshot.searchQuery,
    filter: snapshot.filter,
    namespaceFilter: snapshot.namespaceFilter,
    dirtyIds: snapshot.dirtyIds,
    dirtyRevision: snapshot.dirtyRevision,
    undoStack: snapshot.undoStack,
    redoStack: snapshot.redoStack,
    translatedCount: snapshot.translatedCount,
    needsReviewCount: snapshot.needsReviewCount,
    scrollTop: snapshot.scrollTop,
  };
}

function mergeEntryMap(
  current: Map<number, TranslationEntry>,
  incomingEntries: TranslationEntry[],
) {
  const entries = new Map(current);
  for (const incoming of incomingEntries) {
    const previous = entries.get(incoming.id);
    entries.set(incoming.id, previous?.asset ? { ...incoming, asset: previous.asset } : incoming);
  }
  return entries;
}

function refreshEntryMap(
  current: Map<number, TranslationEntry>,
  incomingEntries: TranslationEntry[],
  dirtyIds: number[],
  baseline?: Map<number, TranslationEntry>,
) {
  const dirty = new Set(dirtyIds);
  const incoming = new Map(incomingEntries.map((entry) => [entry.id, entry]));
  const entries = new Map(current);
  for (const [id, next] of incoming) {
    const previous = current.get(id);
    // 待機中の編集が保存済みになっても、要求開始時から変わった行は古い応答で上書きしない。
    // 行は不変更新されるので参照で判定し、本文が同じ値に戻った編集や保存時のorigin変更も保護する。
    if (dirty.has(id) || (baseline && previous !== baseline.get(id))) continue;
    entries.set(id, previous?.asset ? { ...next, asset: previous.asset } : next);
  }
  return entries;
}

export const useEditorStore = create<EditorStoreState>()((set, get) => ({
  aiHistoryPending: false,
  entries: new Map(),
  glossaryTerms: [],
  glossaryMatcher: null,
  orderedIds: [],
  hasLoadedEntries: false,
  loadedProjectId: null,
  activeModUnitId: null,
  unitStates: new Map(),
  deferredModAiUndoGroups: new Map(),
  scrollTop: 0,
  filteredIds: [],
  namespaces: [],
  selectedIds: [],
  selectedIdSet: new Set(),
  selectionAnchorId: null,
  focusedCell: null,
  aiHighlightedIds: new Set(),
  searchQuery: "",
  filter: "all",
  namespaceFilter: "all",
  dirtyIds: [],
  dirtyRevision: 0,
  undoStack: [],
  redoStack: [],
  translatedCount: 0,
  needsReviewCount: 0,
  saveStatus: "saved",
  saveError: null,
  setGlossaryTerms: (glossaryTerms) =>
    set((state) => {
      const glossaryMatcher = glossaryTerms.length > 0 ? new GlossaryMatcher(glossaryTerms) : null;
      const filteredIds = buildFilteredIds({ ...state, glossaryMatcher });
      const unitStates = new Map(state.unitStates);
      for (const [id, snapshot] of unitStates) {
        const cachedFilteredIds = buildFilteredIds({ ...snapshot, glossaryMatcher });
        unitStates.set(id, {
          ...snapshot,
          filteredIds: cachedFilteredIds,
          focusedCell: focusWithinResults(snapshot.focusedCell, cachedFilteredIds),
        });
      }
      return {
        glossaryTerms,
        glossaryMatcher,
        filteredIds,
        focusedCell: focusWithinResults(state.focusedCell, filteredIds),
        unitStates,
      };
    }),
  loadEntries: (entries, projectId) => {
    const entryMap = new Map(entries.map((entry) => [entry.id, entry]));
    const orderedIds = entries.map((entry) => entry.id);
    const namespaces = [...new Set(entries.map((entry) => entry.namespace))].sort();
    const counts = countsFor(entryMap);
    set((state) => {
      const nextProjectId = projectId ?? null;
      const projectChanged = state.loadedProjectId !== nextProjectId;
      const searchQuery = projectChanged ? "" : state.searchQuery;
      const filter = projectChanged ? "all" : state.filter;
      const namespaceFilter = projectChanged ? "all" : state.namespaceFilter;
      const filteredIds = buildFilteredIds({
        entries: entryMap,
        orderedIds,
        searchQuery,
        filter,
        namespaceFilter,
        glossaryMatcher: state.glossaryMatcher,
      });
      return {
        entries: entryMap,
        orderedIds,
        hasLoadedEntries: true,
        loadedProjectId: nextProjectId,
        activeModUnitId: entries.length > 0 && entries.every((entry) => entry.modUnitId === entries[0]?.modUnitId)
          ? entries[0]?.modUnitId ?? null
          : null,
        unitStates: projectChanged ? new Map() : state.unitStates,
        deferredModAiUndoGroups: projectChanged ? new Map() : state.deferredModAiUndoGroups,
        scrollTop: 0,
        searchQuery,
        filter,
        namespaceFilter,
        namespaces,
        filteredIds,
        selectedIds: [],
        selectedIdSet: new Set(),
        selectionAnchorId: null,
        focusedCell: focusWithinResults(state.focusedCell, filteredIds),
        aiHighlightedIds: new Set(),
        dirtyIds: [],
        undoStack: [],
        redoStack: [],
        translatedCount: counts.translatedCount,
        needsReviewCount: counts.needsReviewCount,
        saveStatus: "saved",
        saveError: null,
      };
    });
  },
  activateLoadedModUnit: (modUnitId, projectId) =>
    set((state) => {
      if (state.loadedProjectId !== projectId || !state.hasLoadedEntries) return state;
      if (state.activeModUnitId === modUnitId) return state;
      if (state.activeModUnitId !== null) return state;
      return { activeModUnitId: modUnitId };
    }),
  activateModUnit: (modUnitId, entries, projectId) =>
    set((state) => {
      if (state.activeModUnitId === modUnitId && state.loadedProjectId === projectId) return state;

      // 編集対象だけを現在のStoreに置き、開いている他modはタブ復帰用snapshotとして保持する。
      const unitStates = new Map(state.loadedProjectId === projectId ? state.unitStates : []);
      if (state.activeModUnitId !== null && state.loadedProjectId === projectId) {
        unitStates.set(state.activeModUnitId, snapshotUnitState(state, state.activeModUnitId, projectId));
      }

      const cached = unitStates.get(modUnitId);
      unitStates.delete(modUnitId);
      const active = cached?.projectId === projectId
        ? cached
        : createUnitSnapshot(modUnitId, entries, projectId);
      let deferredModAiUndoGroups = state.deferredModAiUndoGroups;
      if (!cached || cached.projectId !== projectId) {
        const deferredGroups = state.deferredModAiUndoGroups.get(modUnitId) ?? [];
        for (const deferred of deferredGroups) {
          if (deferred.projectId !== projectId) continue;
          active.undoStack = insertHistoryGroup(active.undoStack, deferred.changes, deferred.historyBoundary);
        }
        if (deferredGroups.length > 0) {
          deferredModAiUndoGroups = new Map(state.deferredModAiUndoGroups);
          deferredModAiUndoGroups.delete(modUnitId);
        }
      }
      return {
        ...unitSnapshotFields(active),
        hasLoadedEntries: true,
        loadedProjectId: projectId,
        activeModUnitId: modUnitId,
        unitStates,
        deferredModAiUndoGroups,
        saveStatus: "saved",
        saveError: null,
      };
    }),
  discardClosedModUnits: (keepIds) =>
    set((state) => {
      const keep = new Set(keepIds);
      const unitStates = new Map([...state.unitStates].filter(([id]) => keep.has(id)));
      return unitStates.size === state.unitStates.size ? state : { unitStates };
    }),
  setScrollTop: (scrollTop) =>
    set((state) => state.scrollTop === scrollTop ? state : { scrollTop }),
  mergeExternalModUnitEntries: (modUnitId, incomingEntries, highlightedIds = []) =>
    set((state) => {
      if (state.activeModUnitId === modUnitId) {
        const entries = mergeEntryMap(state.entries, incomingEntries);
        const counts = countsFor(entries);
        const filteredIds = buildFilteredIds({ ...state, entries });
        const aiHighlightedIds = new Set(state.aiHighlightedIds);
        for (const id of highlightedIds) aiHighlightedIds.add(id);
        return {
          entries,
          filteredIds,
          focusedCell: focusWithinResults(state.focusedCell, filteredIds),
          aiHighlightedIds,
          redoStack: [],
          ...counts,
        };
      }
      const cached = state.unitStates.get(modUnitId);
      if (!cached) return state;
      const entries = mergeEntryMap(cached.entries, incomingEntries);
      const counts = countsFor(entries);
      const filteredIds = buildFilteredIds({ ...cached, entries, glossaryMatcher: state.glossaryMatcher });
      const aiHighlightedIds = new Set(cached.aiHighlightedIds);
      for (const id of highlightedIds) aiHighlightedIds.add(id);
      const unitStates = new Map(state.unitStates);
      unitStates.set(modUnitId, {
        ...cached,
        entries,
        filteredIds,
        focusedCell: focusWithinResults(cached.focusedCell, filteredIds),
        aiHighlightedIds,
        redoStack: [],
        ...counts,
      });
      return { unitStates };
    }),
  finishExternalModUnitAiUndoGroup: (modUnitId, changes, historyBoundary, projectId) =>
    set((state) => {
      if (changes.length === 0) return state;
      if (state.activeModUnitId === modUnitId) {
        return {
          undoStack: insertHistoryGroup(state.undoStack, changes, historyBoundary),
          redoStack: [],
        };
      }
      const cached = state.unitStates.get(modUnitId);
      if (!cached) {
        const deferredModAiUndoGroups = new Map(state.deferredModAiUndoGroups);
        const deferredGroups = deferredModAiUndoGroups.get(modUnitId) ?? [];
        deferredModAiUndoGroups.set(modUnitId, [...deferredGroups, { projectId, changes, historyBoundary }]);
        return { deferredModAiUndoGroups };
      }
      const unitStates = new Map(state.unitStates);
      unitStates.set(modUnitId, {
        ...cached,
        undoStack: insertHistoryGroup(cached.undoStack, changes, historyBoundary),
        redoStack: [],
      });
      return { unitStates };
    }),
  refreshCachedModUnitEntries: (modUnitId, incomingEntries, baseline) =>
    set((state) => {
      if (state.activeModUnitId === modUnitId) {
        const entries = refreshEntryMap(state.entries, incomingEntries, state.dirtyIds, baseline);
        const counts = countsFor(entries);
        const filteredIds = buildFilteredIds({ ...state, entries });
        return {
          entries,
          filteredIds,
          focusedCell: focusWithinResults(state.focusedCell, filteredIds),
          ...counts,
        };
      }
      const cached = state.unitStates.get(modUnitId);
      if (!cached) return state;
      const entries = refreshEntryMap(cached.entries, incomingEntries, cached.dirtyIds, baseline);
      const counts = countsFor(entries);
      const filteredIds = buildFilteredIds({ ...cached, entries, glossaryMatcher: state.glossaryMatcher });
      const unitStates = new Map(state.unitStates);
      unitStates.set(modUnitId, {
        ...cached,
        entries,
        filteredIds,
        focusedCell: focusWithinResults(cached.focusedCell, filteredIds),
        ...counts,
      });
      return { unitStates };
    }),
  setSearchQuery: (searchQuery) =>
    set((state) => {
      const filteredIds = buildFilteredIds({ ...state, searchQuery });
      return { searchQuery, filteredIds, focusedCell: focusWithinResults(state.focusedCell, filteredIds) };
    }),
  setFilter: (filter) =>
    set((state) => {
      const filteredIds = buildFilteredIds({ ...state, filter });
      return { filter, filteredIds, focusedCell: focusWithinResults(state.focusedCell, filteredIds) };
    }),
  setNamespaceFilter: (namespaceFilter) =>
    set((state) => {
      const filteredIds = buildFilteredIds({ ...state, namespaceFilter });
      return { namespaceFilter, filteredIds, focusedCell: focusWithinResults(state.focusedCell, filteredIds) };
    }),
  selectEntry: (entryId, mode = "replace") =>
    set((state) => {
      if (!state.entries.has(entryId)) return state;
      let selectedIds: number[];
      let selectionAnchorId = state.selectionAnchorId;
      if (mode === "toggle") {
        const selected = new Set(state.selectedIds);
        if (selected.has(entryId)) selected.delete(entryId);
        else selected.add(entryId);
        selectedIds = [...selected];
        selectionAnchorId ??= entryId;
      } else if (mode === "range") {
        const anchorId = selectionAnchorId ?? entryId;
        const anchorIndex = state.filteredIds.indexOf(anchorId);
        const currentIndex = state.filteredIds.indexOf(entryId);
        if (anchorIndex < 0 || currentIndex < 0) {
          selectedIds = [entryId];
          selectionAnchorId = entryId;
        } else {
          const [start, end] = [anchorIndex, currentIndex].sort((a, b) => a - b);
          selectedIds = state.filteredIds.slice(start, end + 1);
        }
      } else {
        selectedIds = [entryId];
        selectionAnchorId = entryId;
      }
      return { selectedIds, selectedIdSet: new Set(selectedIds), selectionAnchorId };
    }),
  clearSelection: () =>
    set({ selectedIds: [], selectedIdSet: new Set(), selectionAnchorId: null }),
  setFocusedCell: (focusedCell) => set({ focusedCell }),
  editEntry: (entryId, targetText, timestamp = Date.now(), validator = mockPlaceholderValidator) => {
    const state = get();
    const before = state.entries.get(entryId);
    if (!before || before.targetText === targetText) return;
    const afterFlags = flagsForTarget(before, targetText, validator);
    const change: EntryEdit = {
      entryId,
      before: before.targetText,
      after: targetText,
      beforeOrigin: before.origin,
      afterOrigin: "edited",
      beforeFlags: [...before.flags],
      afterFlags,
      timestamp,
      mergeable: true,
    };
    set((current) => {
      const lastGroup = current.undoStack.at(-1);
      const lastChange = lastGroup?.at(-1);
      const canMerge =
        lastGroup?.length === 1 &&
        lastChange?.mergeable === true &&
        lastChange?.entryId === entryId &&
        timestamp >= lastChange.timestamp &&
        timestamp - lastChange.timestamp <= MERGE_WINDOW_MS &&
        lastChange.after === change.before;
      const nextChange = canMerge
        ? { ...change, before: lastChange.before, beforeOrigin: lastChange.beforeOrigin, beforeFlags: lastChange.beforeFlags }
        : change;
      const undoStack = canMerge
        ? [...current.undoStack.slice(0, -1), [nextChange]]
        : [...current.undoStack, [change]];
      return applyEditsToState(current, [change], undoStack);
    });
  },
  applyBatchEdits: (edits, timestamp = Date.now(), validator = mockPlaceholderValidator) => {
    const state = get();
    const changes: EntryEdit[] = [];
    const seen = new Set<number>();
    for (const edit of edits) {
      if (seen.has(edit.entryId)) continue;
      seen.add(edit.entryId);
      const before = state.entries.get(edit.entryId);
      if (!before || before.targetText === edit.targetText) continue;
      changes.push({
        entryId: edit.entryId,
        before: before.targetText,
        after: edit.targetText,
        beforeOrigin: before.origin,
        afterOrigin: "edited",
        beforeFlags: [...before.flags],
        afterFlags: flagsForTarget(before, edit.targetText, validator),
        timestamp,
      });
    }
    if (changes.length === 0) return;
    set((current) => applyEditsToState(current, changes, [...current.undoStack, changes]));
  },
  pasteTranslations: (targetTexts, startId) => {
    const state = get();
    const startIndex = state.filteredIds.indexOf(
      startId ?? state.selectedIds[0] ?? state.focusedCell?.entryId ?? state.filteredIds[0] ?? -1,
    );
    if (startIndex < 0) return;
    const edits = targetTexts.flatMap((targetText, offset) => {
      const entryId = state.filteredIds[startIndex + offset];
      return entryId === undefined ? [] : [{ entryId, targetText }];
    });
    get().applyBatchEdits(edits);
  },
  applyAiTranslation: (entryId, targetText, timestamp = Date.now(), validator = mockPlaceholderValidator) =>
    get().applyAiTranslations([{ entryId, targetText }], timestamp, validator),
  applyAiTranslations: (edits, timestamp = Date.now(), validator = mockPlaceholderValidator) => {
    const state = get();
    const changes: EntryEdit[] = [];
    const seen = new Set<number>();
    for (const edit of edits) {
      if (seen.has(edit.entryId)) continue;
      seen.add(edit.entryId);
      const before = state.entries.get(edit.entryId);
      if (!before || before.targetText === edit.targetText) continue;
      changes.push({
        entryId: edit.entryId,
        before: before.targetText,
        after: edit.targetText,
        beforeOrigin: before.origin,
        afterOrigin: "ai",
        beforeFlags: [...before.flags],
        afterFlags: flagsForTarget(before, edit.targetText, validator),
        timestamp,
      });
    }
    if (changes.length === 0) return;
    const entryIds = changes.map((change) => change.entryId);
    set((current) => {
      const aiHighlightedIds = new Set(current.aiHighlightedIds);
      for (const id of entryIds) aiHighlightedIds.add(id);
      return {
        ...applyEditsToState(current, changes, [...current.undoStack, changes]),
        aiHighlightedIds,
      };
    });
    for (const entryId of entryIds) {
      const existingTimer = aiHighlightTimers.get(entryId);
      if (existingTimer) clearTimeout(existingTimer);
      aiHighlightTimers.set(
        entryId,
        setTimeout(() => {
          set((current) => {
            const aiHighlightedIds = new Set(current.aiHighlightedIds);
            aiHighlightedIds.delete(entryId);
            return { aiHighlightedIds };
          });
          aiHighlightTimers.delete(entryId);
        }, 220),
      );
    }
  },
  applyExternalAiBatch: (entries, entryIds, highlightedIds = entryIds) => {
    set((state) => {
      const counts = countsFor(entries);
      const filteredIds = buildFilteredIds({ ...state, entries });
      const aiHighlightedIds = new Set(state.aiHighlightedIds);
      for (const id of highlightedIds) aiHighlightedIds.add(id);
      return {
        entries,
        filteredIds,
        focusedCell: focusWithinResults(state.focusedCell, filteredIds),
        aiHighlightedIds,
        redoStack: [],
        translatedCount: counts.translatedCount,
        needsReviewCount: counts.needsReviewCount,
      };
    });
    for (const entryId of highlightedIds) {
      const existingTimer = aiHighlightTimers.get(entryId);
      if (existingTimer) clearTimeout(existingTimer);
      aiHighlightTimers.set(
        entryId,
        setTimeout(() => {
          set((current) => {
            const aiHighlightedIds = new Set(current.aiHighlightedIds);
            aiHighlightedIds.delete(entryId);
            return { aiHighlightedIds };
          });
          aiHighlightTimers.delete(entryId);
        }, 220),
      );
    }
  },
  finishAiUndoGroup: (changes, historyBoundary) => {
    if (changes.length === 0) return;
    set((state) => {
      return {
        undoStack: historyBoundary
          ? insertHistoryGroup(state.undoStack, changes, historyBoundary)
          : [...state.undoStack, changes],
        redoStack: [],
      };
    });
  },
  undo: () =>
    set((state) => {
      if (state.aiHistoryPending) return state;
      const group = state.undoStack.at(-1);
      if (!group) return state;
      const entries = applyChangeGroup(state.entries, [...group].reverse(), "undo");
      return stateAfterHistoryMove(
        state,
        entries,
        state.undoStack.slice(0, -1),
        [...state.redoStack, group],
        group,
      );
    }),
  redo: () =>
    set((state) => {
      if (state.aiHistoryPending) return state;
      const group = state.redoStack.at(-1);
      if (!group) return state;
      const entries = applyChangeGroup(state.entries, group, "redo");
      return stateAfterHistoryMove(
        state,
        entries,
        [...state.undoStack, group],
        state.redoStack.slice(0, -1),
        group,
      );
    }),
  getDirtyEntries: () => {
    const state = get();
    return state.dirtyIds.flatMap((id) => {
      const entry = state.entries.get(id);
      return entry ? [entry] : [];
    });
  },
  markSaved: (savedEntries) =>
    set((state) => {
      const serverEntries = new Map(savedEntries.map((entry) => [entry.id, entry]));
      const entries = new Map(state.entries);
      const dirtyIds = state.dirtyIds.filter((id) => {
        const current = entries.get(id);
        const saved = serverEntries.get(id);
        if (!current || !saved || current.targetText !== saved.targetText) {
          return true;
        }
        // Rustは手動保存のoriginをeditedに確定する。本文が同じなら保存済みoriginと検証flagを採用し、Undo後の再保存ループを防ぐ。
        entries.set(id, { ...current, origin: saved.origin, flags: [...saved.flags] });
        return false;
      });
      const counts = countsFor(entries);
      const filteredIds = buildFilteredIds({ ...state, entries });
      return {
        entries,
        filteredIds,
        focusedCell: focusWithinResults(state.focusedCell, filteredIds),
        dirtyIds,
        saveStatus: dirtyIds.length > 0 ? "saving" : "saved",
        saveError: null,
        translatedCount: counts.translatedCount,
        needsReviewCount: counts.needsReviewCount,
      };
    }),
  markSaveError: () => set({ saveStatus: "error", saveError: "保存できませんでした" }),
  setSaveStatus: (saveStatus, saveError = null) => set({ saveStatus, saveError }),
}));

function applyEditsToState(
  state: EditorStoreState,
  changes: EntryEdit[],
  undoStack: EntryEdit[][],
): Partial<EditorStoreState> {
  const entries = applyChangeGroup(state.entries, changes, "redo");
  const translatedCount =
    state.translatedCount +
    changes.reduce((count, change) => {
      const previous = state.entries.get(change.entryId);
      const next = entries.get(change.entryId);
      return count + Number(Boolean(next && isTranslated(next))) - Number(Boolean(previous && isTranslated(previous)));
    }, 0);
  const needsReviewCount =
    state.needsReviewCount +
    changes.reduce((count, change) => {
      const previous = state.entries.get(change.entryId);
      const next = entries.get(change.entryId);
      return (
        count +
        Number(Boolean(next?.flags.includes("needs_review"))) -
        Number(Boolean(previous?.flags.includes("needs_review")))
      );
    }, 0);
  const filteredIds = buildFilteredIds({ ...state, entries });
  return {
    entries,
    filteredIds,
    focusedCell: focusWithinResults(state.focusedCell, filteredIds),
    undoStack,
    redoStack: [],
    dirtyIds: uniqueDirtyIds(state.dirtyIds, changes),
    dirtyRevision: state.dirtyRevision + 1,
    translatedCount,
    needsReviewCount,
    saveStatus: state.saveStatus === "error" ? "saved" : state.saveStatus,
    saveError: null,
  };
}

function stateAfterHistoryMove(
  state: EditorStoreState,
  entries: Map<number, TranslationEntry>,
  undoStack: EntryEdit[][],
  redoStack: EntryEdit[][],
  movedGroup: EntryEdit[],
): Partial<EditorStoreState> {
  const translatedCount =
    state.translatedCount +
    movedGroup.reduce((count, change) => {
      const previous = state.entries.get(change.entryId);
      const next = entries.get(change.entryId);
      return count + Number(Boolean(next && isTranslated(next))) - Number(Boolean(previous && isTranslated(previous)));
    }, 0);
  const needsReviewCount =
    state.needsReviewCount +
    movedGroup.reduce((count, change) => {
      const previous = state.entries.get(change.entryId);
      const next = entries.get(change.entryId);
      return (
        count +
        Number(Boolean(next?.flags.includes("needs_review"))) -
        Number(Boolean(previous?.flags.includes("needs_review")))
      );
    }, 0);
  const filteredIds = buildFilteredIds({ ...state, entries });
  return {
    entries,
    filteredIds,
    focusedCell: focusWithinResults(state.focusedCell, filteredIds),
    undoStack,
    redoStack,
    dirtyIds: uniqueDirtyIds(state.dirtyIds, movedGroup),
    dirtyRevision: state.dirtyRevision + 1,
    translatedCount,
    needsReviewCount,
    saveStatus: state.saveStatus === "error" ? "saved" : state.saveStatus,
    saveError: null,
  };
}
