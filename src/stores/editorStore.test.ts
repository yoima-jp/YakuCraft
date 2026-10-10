import { beforeEach, describe, expect, it } from "vitest";

import { useEditorStore } from "./editorStore";
import type { TranslationEntry } from "../types/translation";

function entries(): TranslationEntry[] {
  return [
    {
      id: 1,
      namespace: "first",
      key: "item.apple",
      sourceText: "Apple %s",
      sourceLocale: "en_us",
      targetText: "りんご %s",
      origin: "existing",
      flags: [],
    },
    {
      id: 2,
      namespace: "second",
      key: "block.stone",
      sourceText: "Stone block",
      sourceLocale: "en_us",
      targetText: "石ブロック",
      origin: "ai",
      flags: ["needs_review"],
    },
    {
      id: 3,
      namespace: "second",
      key: "item.empty",
      sourceText: "Empty item",
      sourceLocale: "en_gb",
      targetText: "",
      origin: "missing",
      flags: ["source_fallback"],
    },
  ];
}

describe("editorStore", () => {
  beforeEach(() => useEditorStore.getState().loadEntries(entries()));

  it("手動編集でoriginとplaceholder warningを更新し、undo/redoで戻す", () => {
    const store = useEditorStore.getState();
    store.editEntry(1, "りんご %d", 1000);

    expect(useEditorStore.getState().entries.get(1)).toMatchObject({
      targetText: "りんご %d",
      origin: "edited",
      flags: ["placeholder_error"],
    });
    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)).toMatchObject({
      targetText: "りんご %s",
      origin: "existing",
      flags: [],
    });
    useEditorStore.getState().redo();
    expect(useEditorStore.getState().entries.get(1)?.origin).toBe("edited");
  });

  it("連続入力をmergeし、pasteは複数行を1回でundoする", () => {
    const store = useEditorStore.getState();
    store.editEntry(1, "りんご %s!", 1000);
    useEditorStore.getState().editEntry(1, "りんご %s!!", 1300);
    expect(useEditorStore.getState().undoStack).toHaveLength(1);
    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("りんご %s");

    useEditorStore.getState().pasteTranslations(["リンゴ", "石"] , 1);
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("リンゴ");
    expect(useEditorStore.getState().entries.get(2)?.targetText).toBe("石");
    expect(useEditorStore.getState().undoStack).toHaveLength(1);
    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("りんご %s");
    expect(useEditorStore.getState().entries.get(2)?.targetText).toBe("石ブロック");
  });

  it("AIの一括反映を1グループでundoし、対象行をhighlightする", () => {
    useEditorStore.getState().applyAiTranslations([
      { entryId: 1, targetText: "りんごAI %s" },
      { entryId: 2, targetText: "石AI" },
    ]);
    expect(useEditorStore.getState().undoStack).toHaveLength(1);
    expect(useEditorStore.getState().entries.get(1)?.origin).toBe("ai");
    expect(useEditorStore.getState().entries.get(2)?.origin).toBe("ai");
    expect([...useEditorStore.getState().aiHighlightedIds]).toEqual([1, 2]);

    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)?.origin).toBe("existing");
    expect(useEditorStore.getState().entries.get(2)?.origin).toBe("ai");
  });

  it("複数バッチのDB反映を1つのUndoグループにまとめ、Undoを保存対象にする", () => {
    const changes = [1, 3].map((entryId) => {
      const current = useEditorStore.getState().entries.get(entryId)!;
      const targetText = `AI訳${entryId}`;
      const after = { ...current, targetText, origin: "ai" as const, flags: [...current.flags] };
      const next = new Map(useEditorStore.getState().entries);
      next.set(entryId, after);
      useEditorStore.getState().applyExternalAiBatch(next, [entryId]);
      return {
        entryId,
        before: current.targetText,
        after: targetText,
        beforeOrigin: current.origin,
        afterOrigin: "ai" as const,
        beforeFlags: [...current.flags],
        afterFlags: [...after.flags],
        timestamp: Date.now(),
      };
    });

    expect(useEditorStore.getState().dirtyIds).toEqual([]);
    useEditorStore.getState().finishAiUndoGroup(changes);
    expect(useEditorStore.getState().undoStack).toHaveLength(1);
    expect(useEditorStore.getState().undoStack[0]).toHaveLength(2);

    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("りんご %s");
    expect(useEditorStore.getState().entries.get(3)?.targetText).toBe("");
    expect(useEditorStore.getState().dirtyIds).toEqual([1, 3]);
  });

  it("検索対象とstatus・namespace filterを組み合わせる", () => {
    const store = useEditorStore.getState();
    store.setSearchQuery("STONE");
    expect(useEditorStore.getState().filteredIds).toEqual([2]);

    useEditorStore.getState().setSearchQuery("");
    useEditorStore.getState().setFilter("needs_review");
    expect(useEditorStore.getState().filteredIds).toEqual([2]);
    useEditorStore.getState().setFilter("missing");
    expect(useEditorStore.getState().filteredIds).toEqual([3]);
    useEditorStore.getState().setFilter("all");
    useEditorStore.getState().setNamespaceFilter("first");
    expect(useEditorStore.getState().filteredIds).toEqual([1]);
    useEditorStore.getState().setSearchQuery("apple");
    expect(useEditorStore.getState().filteredIds).toEqual([1]);
  });

  it.each([false, true])("要求開始後の保存済み変更を再読込から保護する（非表示mod=%s）", (switchUnit) => {
    useEditorStore.getState().loadEntries(entries(), "project-1");
    useEditorStore.getState().activateLoadedModUnit(11, "project-1");
    const baseline = useEditorStore.getState().entries;
    useEditorStore.getState().editEntry(1, "保存した訳 %s");
    useEditorStore.getState().markSaved(useEditorStore.getState().getDirtyEntries());
    if (switchUnit) useEditorStore.getState().activateModUnit(22, [], "project-1");
    useEditorStore.getState().refreshCachedModUnitEntries(11, [
      { ...entries()[0]!, targetText: "古いAI訳 %s", origin: "ai" },
      { ...entries()[1]!, targetText: "最新AI訳", origin: "ai" },
    ], baseline);
    if (switchUnit) useEditorStore.getState().activateModUnit(11, [], "project-1");
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("保存した訳 %s");
    expect(useEditorStore.getState().entries.get(2)?.targetText).toBe("最新AI訳");
    expect(useEditorStore.getState().dirtyIds).toEqual([]);
  });

  it("modを切り替えて戻ると検索、選択、スクロール、Undo履歴を復元する", () => {
    const firstUnitEntries = entries().map((entry) => ({ ...entry, modUnitId: 11 }));
    useEditorStore.getState().loadEntries(firstUnitEntries, "project-tabs");
    useEditorStore.getState().setSearchQuery("apple");
    useEditorStore.getState().setNamespaceFilter("first");
    useEditorStore.getState().selectEntry(1);
    useEditorStore.getState().editEntry(1, "りんご改 %s", 1000);
    useEditorStore.getState().setScrollTop(264);

    const secondEntry: TranslationEntry = {
      id: 4,
      modUnitId: 22,
      namespace: "other",
      key: "item.other",
      sourceText: "Other",
      sourceLocale: "en_us",
      targetText: "別の項目",
      origin: "existing",
      flags: [],
    };
    useEditorStore.getState().activateModUnit(22, [secondEntry], "project-tabs");
    expect(useEditorStore.getState().searchQuery).toBe("");
    useEditorStore.getState().setSearchQuery("other");

    useEditorStore.getState().activateModUnit(11, [], "project-tabs");
    const restored = useEditorStore.getState();
    expect(restored.searchQuery).toBe("apple");
    expect(restored.namespaceFilter).toBe("first");
    expect(restored.filteredIds).toEqual([1]);
    expect(restored.selectedIds).toEqual([1]);
    expect(restored.scrollTop).toBe(264);
    expect(restored.entries.get(1)?.targetText).toBe("りんご改 %s");
    expect(restored.undoStack).toHaveLength(1);

    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("りんご %s");
  });
});
