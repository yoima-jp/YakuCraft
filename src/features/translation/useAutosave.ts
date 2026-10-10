import { useCallback, useEffect, useRef } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "@tauri-apps/api/core";

import type { EditorPersistence } from "./persistence";
import { useEditorStore } from "../../stores/editorStore";
import { registerEditorFlush } from "../../lib/navigation";
import { exitApp } from "../../lib/tauri";
import { commitPendingDrafts } from "./pendingDrafts";

export function useAutosave(persistence: EditorPersistence, debounceMs = 250) {
  const dirtyRevision = useEditorStore((state) => state.dirtyRevision);
  const saveStatus = useEditorStore((state) => state.saveStatus);
  const inFlight = useRef<Promise<boolean> | null>(null);

  const savePending = useCallback(async function savePendingEntries(commitDrafts: boolean): Promise<boolean> {
    // タイマー保存で別セルの入力やIME変換を閉じない。下書きの確定は画面離脱などの明示flushだけで行う。
    if (commitDrafts) commitPendingDrafts();
    const existing = inFlight.current;
    if (existing) {
      await existing;
      // 明示flushでは、保存待ちの間に始めた入力も離脱前に確定する。
      if (commitDrafts) commitPendingDrafts();
      const state = useEditorStore.getState();
      if (state.saveStatus === "error") return false;
      if (state.dirtyIds.length > 0) {
        return savePendingEntries(commitDrafts);
      }
      return true;
    }

    const state = useEditorStore.getState();
    const snapshot = state.getDirtyEntries();
    if (snapshot.length === 0) return state.dirtyIds.length === 0;

    state.setSaveStatus("saving");
    const request = (async () => {
      try {
        const savedEntries = await persistence.save(snapshot);
        useEditorStore.getState().markSaved(savedEntries);
        return true;
      } catch {
        useEditorStore.getState().markSaveError();
        return false;
      }
    })();
    inFlight.current = request;
    const saved = await request;
    // 明示flushだけが、request開始後に入力された下書きも次の保存対象へ含める。
    if (commitDrafts) commitPendingDrafts();
    if (inFlight.current === request) inFlight.current = null;
    if (!saved) return false;

    const afterSave = useEditorStore.getState();
    if (afterSave.dirtyIds.length > 0) {
      return savePendingEntries(commitDrafts);
    }
    return true;
  }, [persistence]);

  const flush = useCallback(() => savePending(true), [savePending]);

  useEffect(() => {
    if (
      dirtyRevision === 0 ||
      saveStatus !== "saved" ||
      useEditorStore.getState().dirtyIds.length === 0
    ) {
      return;
    }
    const timer = window.setTimeout(() => void savePending(false), debounceMs);
    return () => window.clearTimeout(timer);
  }, [debounceMs, dirtyRevision, savePending, saveStatus]);

  useEffect(() => {
    const flushOnBlur = () => void flush();
    window.addEventListener("blur", flushOnBlur);
    return () => window.removeEventListener("blur", flushOnBlur);
  }, [flush]);

  useEffect(() => () => void flush(), [flush]);

  useEffect(() => registerEditorFlush(flush), [flush]);

  useEffect(() => {
    if (!isTauri()) return;
    let allowClose = false;
    let cancelled = false;
    const appWindow = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    void appWindow
      .onCloseRequested(async (event) => {
        if (allowClose) return;
        event.preventDefault();
        if (!(await flush())) return;
        allowClose = true;
        try {
          await exitApp();
        } catch {
          allowClose = false;
        }
      })
      .then((stopListening) => {
        if (cancelled) stopListening();
        else unlisten = stopListening;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [flush]);

  const retry = useCallback(() => void flush(), [flush]);
  return { retry, flush };
}
