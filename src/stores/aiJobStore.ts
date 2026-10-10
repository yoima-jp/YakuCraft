import { create } from "zustand";

import { entryFromProject } from "../lib/tauri";
import type { AiJobEventDto } from "../types/ai";
import type { TranslationEntry } from "../types/translation";
import type { EntryEdit } from "./editorStore";
import { useEditorStore } from "./editorStore";

interface AiJobProgress {
  id: string | null;
  provider: string;
  completed: number;
  total: number;
}

interface AiJobStore {
  activeJob: AiJobProgress | null;
  warnings: Map<number, string[]>;
  notice: string | null;
  projectId: string | null;
  jobProjectId: string | null;
  jobProjectInvalidated: boolean;
  baseline: Map<number, Pick<TranslationEntry, "targetText" | "origin" | "flags">>;
  pendingEdits: EntryEdit[];
  undoBoundary: EntryEdit[][];
  begin: (provider: string, entryIds: number[], total: number, projectId: string) => boolean;
  attachJobId: (jobId: string) => void;
  receive: (event: AiJobEventDto) => void;
  failToStart: (message: string) => void;
  clearNotice: () => void;
  setProject: (projectId: string) => void;
}

function flagsMatch(left: string[], right: string[]) {
  return left.length === right.length && left.every((flag, index) => flag === right[index]);
}

export const useAiJobStore = create<AiJobStore>()((set, get) => ({
  activeJob: null,
  warnings: new Map(),
  notice: null,
  projectId: null,
  jobProjectId: null,
  jobProjectInvalidated: false,
  baseline: new Map(),
  pendingEdits: [],
  undoBoundary: [],
  begin: (provider, entryIds, total, projectId) => {
    if (get().activeJob) return false;
    const entries = useEditorStore.getState().entries;
    const baseline = new Map<number, Pick<TranslationEntry, "targetText" | "origin" | "flags">>();
    for (const id of entryIds) {
      const entry = entries.get(id);
      if (entry) baseline.set(id, { targetText: entry.targetText, origin: entry.origin, flags: [...entry.flags] });
    }
    if (baseline.size === 0) return false;
    const undoBoundary = useEditorStore.getState().undoStack.slice();
    set((state) => ({
      activeJob: { id: null, provider, completed: 0, total },
      baseline,
      pendingEdits: [],
      undoBoundary,
      notice: null,
      projectId,
      jobProjectId: projectId,
      jobProjectInvalidated: false,
      warnings: state.projectId === projectId ? state.warnings : new Map(),
    }));
    return true;
  },
  attachJobId: (id) => set((state) => state.activeJob ? { activeJob: { ...state.activeJob, id } } : state),
  receive: (event) => {
    const state = get();
    if (!state.activeJob || (state.activeJob.id && state.activeJob.id !== event.jobId)) return;
    if (event.kind === "progress") {
      if (state.jobProjectInvalidated || state.jobProjectId !== state.projectId) return;
      set({ activeJob: { ...state.activeJob, id: event.jobId, completed: event.completed, total: event.total } });
      return;
    }
    if (event.kind === "batchResult") {
      const editor = useEditorStore.getState();
      if (state.jobProjectInvalidated || state.jobProjectId !== state.projectId || editor.loadedProjectId !== state.jobProjectId) return;
      const nextEntries = new Map(editor.entries);
      const changes = [...state.pendingEdits];
      const acceptedIds = new Set<number>();
      const rejectedIds = new Set<number>();
      for (const dto of event.entries) {
        const incoming = entryFromProject(dto);
        const original = state.baseline.get(incoming.id);
        const current = nextEntries.get(incoming.id);
        if (
          !original || !current || current.targetText !== original.targetText ||
          current.origin !== original.origin || !flagsMatch(current.flags, original.flags)
        ) {
          continue;
        }
        if (
          current.targetText === incoming.targetText && current.origin === incoming.origin &&
          flagsMatch(current.flags, incoming.flags)
        ) {
          acceptedIds.add(incoming.id);
          continue;
        }
        const change: EntryEdit = {
          entryId: incoming.id,
          before: current.targetText,
          after: incoming.targetText,
          beforeOrigin: current.origin,
          afterOrigin: "ai",
          beforeFlags: [...current.flags],
          afterFlags: [...incoming.flags],
          timestamp: Date.now(),
        };
        changes.push(change);
        nextEntries.set(incoming.id, { ...current, targetText: incoming.targetText, origin: "ai", flags: [...incoming.flags] });
        acceptedIds.add(incoming.id);
      }

      const warnings = new Map(state.warnings);
      for (const id of acceptedIds) warnings.delete(id);
      for (const rejection of event.rejected) {
        const current = nextEntries.get(rejection.id);
        const original = state.baseline.get(rejection.id);
        if (
          !current || !original || current.targetText !== original.targetText ||
          current.origin !== original.origin || !flagsMatch(current.flags, original.flags)
        ) continue;
        nextEntries.set(rejection.id, {
          ...current,
          flags: current.flags.includes("needs_review") ? current.flags : [...current.flags, "needs_review"],
        });
        rejectedIds.add(rejection.id);
        warnings.set(rejection.id, rejection.reasons.length > 0 ? rejection.reasons : ["翻訳結果を確認してください"]);
      }

      const changedIds = [...acceptedIds, ...rejectedIds];
      if (changedIds.length > 0) {
        editor.applyExternalAiBatch(nextEntries, changedIds, [...acceptedIds]);
      }
      set({
        activeJob: { ...state.activeJob, id: event.jobId },
        pendingEdits: changes,
        warnings,
      });
      return;
    }

    const editor = useEditorStore.getState();
    if (!state.jobProjectInvalidated && state.jobProjectId === state.projectId && editor.loadedProjectId === state.jobProjectId && state.pendingEdits.length > 0) {
      editor.finishAiUndoGroup(state.pendingEdits, state.undoBoundary);
    }
    if (event.kind === "failed") {
      set({ activeJob: null, jobProjectId: null, jobProjectInvalidated: false, baseline: new Map(), pendingEdits: [], undoBoundary: [], notice: event.error.userMessage });
    } else if (event.kind === "cancelled") {
      set({ activeJob: null, jobProjectId: null, jobProjectInvalidated: false, baseline: new Map(), pendingEdits: [], undoBoundary: [], notice: "AI翻訳をキャンセルしました。" });
    } else {
      set({ activeJob: null, jobProjectId: null, jobProjectInvalidated: false, baseline: new Map(), pendingEdits: [], undoBoundary: [], notice: null });
    }
  },
  failToStart: (message) => set({ activeJob: null, jobProjectId: null, jobProjectInvalidated: false, baseline: new Map(), pendingEdits: [], undoBoundary: [], notice: message }),
  clearNotice: () => set({ notice: null }),
  setProject: (projectId) => set((state) => state.projectId === projectId ? state : {
    projectId,
    jobProjectInvalidated: state.jobProjectInvalidated || Boolean(state.activeJob && state.jobProjectId !== projectId),
    warnings: new Map(),
  }),
}));

// AI結果はバッチごとに表示するが履歴は終端でまとめるため、それまでは過去の手入力をUndoさせない。
// 全体翻訳のsetState経路、開始失敗、キャンセルも同じロックに揃える。
useAiJobStore.subscribe((state, previous) => {
  if (Boolean(state.activeJob) !== Boolean(previous.activeJob)) {
    useEditorStore.setState({ aiHistoryPending: Boolean(state.activeJob) });
  }
});
