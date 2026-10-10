import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useEditorStore } from "../../stores/editorStore";
import type { TranslationEntry } from "../../types/translation";
import { MockEditorPersistence } from "./persistence";
import { EditorToolbar } from "./EditorToolbar";
import { useAutosave } from "./useAutosave";
import { registerPendingDraftCommit } from "./pendingDrafts";
import { flushBeforeLeavingEditor } from "../../lib/navigation";

function entry(): TranslationEntry {
  return {
    id: 1,
    namespace: "sample",
    key: "item.apple",
    sourceText: "Apple",
    sourceLocale: "en_us",
    targetText: "りんご",
    origin: "existing",
    flags: [],
  };
}

function SaveHarness({ persistence, debounceMs = 250 }: { persistence: MockEditorPersistence; debounceMs?: number }) {
  const searchRef = useRef<HTMLInputElement>(null);
  const { retry } = useAutosave(persistence, debounceMs);
  const status = useEditorStore((state) => state.saveStatus);
  return (
    <div>
      <EditorToolbar
        searchRef={searchRef}
        onExport={() => {}}
        onToggleInspector={() => {}}
        inspectorOpen
        onRetrySave={retry}
        saveStatus={status}
        requestConfirmation={async () => false}
        onOpenGlossary={() => undefined}
      />
    </div>
  );
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

beforeEach(() => useEditorStore.getState().loadEntries([entry()]));

describe("editor autosave", () => {
  it("250ms debounce後にdirty entryを保存する", async () => {
    vi.useFakeTimers();
    const persistence = new MockEditorPersistence({ delayMs: 0 });
    render(<SaveHarness persistence={persistence} />);
    act(() => useEditorStore.getState().editEntry(1, "りんご!", 1000));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(249);
    });
    expect(persistence.savedEntries.size).toBe(0);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });

    expect(persistence.savedEntries.get(1)?.targetText).toBe("りんご!");
    expect(useEditorStore.getState().dirtyIds).toEqual([]);
    expect(screen.getByText("保存済み")).toBeTruthy();
  });

  it("保存失敗を表示し、再試行で回復する", async () => {
    const persistence = new MockEditorPersistence({ delayMs: 0, failuresRemaining: 1 });
    render(<SaveHarness persistence={persistence} debounceMs={0} />);
    act(() => useEditorStore.getState().editEntry(1, "保存する訳", 1000));
    expect(await screen.findByText("保存できませんでした")).toBeTruthy();
    expect(useEditorStore.getState().dirtyIds).toEqual([1]);

    fireEvent.click(screen.getByRole("button", { name: "再試行" }));
    expect(await screen.findByText("保存済み")).toBeTruthy();
    expect(persistence.savedEntries.get(1)?.targetText).toBe("保存する訳");
  });

  it("タイマー保存と保存完了は別セルの下書きを確定せず、画面離脱では保存する", async () => {
    vi.useFakeTimers();
    useEditorStore.getState().loadEntries([entry(), { ...entry(), id: 2, key: "item.pear" }]);
    const persistence = new MockEditorPersistence({ delayMs: 100 });
    render(<SaveHarness persistence={persistence} />);
    act(() => useEditorStore.getState().editEntry(1, "セルAの訳"));
    const commitDraft = vi.fn(() => useEditorStore.getState().editEntry(2, "入力中のセルB"));
    const unregister = registerPendingDraftCommit(commitDraft);
    try {
      await act(async () => { await vi.advanceTimersByTimeAsync(250); });
      expect(commitDraft).not.toHaveBeenCalled();
      await act(async () => { await vi.advanceTimersByTimeAsync(100); });
      expect(commitDraft).not.toHaveBeenCalled();
      expect(persistence.savedEntries.get(1)?.targetText).toBe("セルAの訳");
      expect(persistence.savedEntries.has(2)).toBe(false);

      let flushed = false;
      await act(async () => {
        const leaving = flushBeforeLeavingEditor();
        await vi.advanceTimersByTimeAsync(100);
        flushed = await leaving;
      });
      expect(flushed).toBe(true);
      expect(commitDraft).toHaveBeenCalled();
      expect(persistence.savedEntries.get(2)?.targetText).toBe("入力中のセルB");
    } finally {
      unregister();
    }
  });

  it("自動保存中の画面離脱は別セルの下書きも保存完了まで待つ", async () => {
    vi.useFakeTimers();
    useEditorStore.getState().loadEntries([entry(), { ...entry(), id: 2, key: "item.pear" }]);
    const persistence = new MockEditorPersistence({ delayMs: 100 });
    render(<SaveHarness persistence={persistence} />);
    act(() => useEditorStore.getState().editEntry(1, "保存中のセルA"));
    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    const unregister = registerPendingDraftCommit(() => useEditorStore.getState().editEntry(2, "離脱前のセルB"));
    try {
      let flushed = false;
      await act(async () => {
        const leaving = flushBeforeLeavingEditor();
        await vi.advanceTimersByTimeAsync(200);
        flushed = await leaving;
      });
      expect(flushed).toBe(true);
      expect(persistence.savedEntries.get(1)?.targetText).toBe("保存中のセルA");
      expect(persistence.savedEntries.get(2)?.targetText).toBe("離脱前のセルB");
      expect(useEditorStore.getState().dirtyIds).toEqual([]);
    } finally {
      unregister();
    }
  });
});
