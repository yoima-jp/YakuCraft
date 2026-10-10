import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getAiSettings, listEntries, listModUnits, saveSettings, startAiJob } from "../../lib/tauri";
import type { RequestConfirmation } from "../../components/useDialogConfirmation";
import { translateSelectedWithAi, translateWholeProjectWithAi, translateWithAi } from "./aiTranslation";
import { EditorToolbar } from "./EditorToolbar";
import { Inspector } from "./Inspector";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useAppStore } from "../../stores/appStore";
import { useEditorStore } from "../../stores/editorStore";
import { useProjectStore } from "../../stores/projectStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import type { AiJobEventDto } from "../../types/ai";
import type { ProjectDetailsDto } from "../../types/project";
import type { TranslationEntry } from "../../types/translation";

vi.mock("../../lib/tauri", () => ({
  AppCommandError: class extends Error {},
  entryFromProject: (entry: {
    id: number;
    modUnitId?: number;
    namespace: string;
    key: string;
    sourceText: string | null;
    sourceLocale: string | null;
    targetText: string | null;
    origin: string;
    flags: string[];
  }) => ({
    id: entry.id,
    modUnitId: entry.modUnitId,
    namespace: entry.namespace,
    key: entry.key,
    sourceText: entry.sourceText,
    sourceLocale: entry.sourceLocale,
    targetText: entry.targetText ?? "",
    origin: entry.origin,
    flags: entry.flags,
  }),
  getAiSettings: vi.fn(),
  listEntries: vi.fn(),
  listModUnits: vi.fn(),
  saveSettings: vi.fn(),
  startAiJob: vi.fn(),
}));

const translatedEntry: TranslationEntry = {
  id: 1,
  namespace: "sample",
  key: "item.apple",
  sourceText: "Apple",
  sourceLocale: "en_us",
  targetText: "りんご",
  origin: "existing",
  flags: [],
};

const requestConfirmation = vi.fn<RequestConfirmation>();

function aiBatch(): AiJobEventDto {
  return {
    kind: "batchResult", jobId: "job-1", rejected: [],
    entries: [{
      ...translatedEntry, modUnitId: 1, targetText: "AIの訳", origin: "ai",
      targetOriginalText: null, translatedSourceText: "Apple", sortIndex: 0,
    }],
  };
}

const projectDetails = {
  project: {
    id: "project-1",
    displayName: "sample.jar",
    modCount: 1,
    instancePath: null,
    createdAt: "2026-09-30T00:00:00Z",
    updatedAt: "2026-09-30T00:00:00Z",
  },
  mods: [{ modUnitId: 1, modId: "sample", name: "Sample Mod", version: "1.0", loader: "Fabric", minecraftVersion: "1.21" }],
  namespaces: [{ modUnitId: 1, namespace: "sample", containerPath: "", included: true, entryCount: 1 }],
  nestedArchives: [],
  languages: [],
  recommendedSourceLocale: "en_us",
  sourceLocale: "en_us",
  fallbackPriority: [],
  preserveExistingJapanese: true,
  fillMissingFromOtherLanguages: true,
  includedNamespaces: [{ modUnitId: 1, namespace: "sample" }],
  setupCompleted: true,
  existingJapaneseTranslationCount: 0,
  missingSourceKeysCount: 0,
  counts: { total: 1, translated: 1, missing: 0, needsReview: 0 },
  targetLanguageFileName: "ja_jp.json",
  targetLanguageFormat: "json",
  isSigned: false,
  assetCount: 0,
  assetSummary: { total: 0, blockModels: 0, itemModels: 0, entityTextures: 0, geometryModels: 0, unresolvedEntries: 0 },
} satisfies ProjectDetailsDto;

describe("AI translation actions", () => {
  afterEach(cleanup);

  beforeEach(() => {
    useEditorStore.getState().loadEntries([translatedEntry], "project-1");
    useProjectStore.setState({ details: projectDetails });
    useAiJobStore.setState({
      activeJob: null,
      warnings: new Map(),
      notice: null,
      pendingEdits: [],
      baseline: new Map(),
      projectId: "project-1",
    });
    useSettingsStore.setState({
      settings: {
        schemaVersion: 1,
        ...SETTING_DEFAULTS,
        "ai.privacyNoticeAccepted": true,
        "editor.confirmBatchOverwrite": true,
      },
      loaded: true,
      error: null,
    });
    useAppStore.setState({
      route: "editor",
      settingsReturnRoute: "editor",
      settingsCategory: "general",
      openSettings: vi.fn(),
    });
    vi.mocked(getAiSettings).mockResolvedValue({
      provider: "openai",
      configured: true,
      model: "example-model",
      baseUrl: "https://api.openai.com/v1",
      batchSize: 50,
      concurrency: 2,
      customPrompt: "",
    });
    vi.mocked(startAiJob).mockResolvedValue("job-1");
    vi.mocked(saveSettings).mockResolvedValue();
    requestConfirmation.mockReset();
    requestConfirmation.mockResolvedValue(false);
  });

  it("Inspectorは3件選択時に3件を翻訳対象にする", async () => {
    const selectedEntries = [1, 2, 3].map((id) => ({
      ...translatedEntry,
      id,
      key: `item.apple.${id}`,
      sourceText: `Apple ${id}`,
      targetText: "",
      origin: "missing" as const,
    }));
    useEditorStore.getState().loadEntries(selectedEntries, "project-1");
    const translateFromInspector = (id: number) => void translateSelectedWithAi(requestConfirmation, id);
    render(createElement(Inspector, { entryId: 1, open: true, onClose: vi.fn(), onTranslate: translateFromInspector }));

    expect(screen.getByRole("button", { name: "この項目を翻訳" })).toBeTruthy();
    act(() => {
      useEditorStore.getState().selectEntry(1);
      useEditorStore.getState().selectEntry(3, "range");
    });
    expect(useEditorStore.getState().selectedIds).toEqual([1, 2, 3]);
    expect(screen.getByRole("button", { name: "選択した3件を翻訳" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "選択した3件を翻訳" }));
    await waitFor(() => expect(startAiJob).toHaveBeenCalledOnce());
    expect(vi.mocked(startAiJob).mock.calls[0]?.[0]).toEqual([1, 2, 3]);
  });

  it("既訳のAI上書き前に確認し、拒否時はジョブを開始しない", async () => {
    await translateWithAi([1], requestConfirmation);

    expect(requestConfirmation).toHaveBeenCalledWith({
      title: "既存の翻訳を上書きしますか？",
      message: "1件の翻訳が置き換わります。",
      confirmLabel: "上書き",
    });
    expect(startAiJob).not.toHaveBeenCalled();
  });

  it("上書きの確認後だけ選択項目のジョブを開始する", async () => {
    requestConfirmation.mockResolvedValue(true);

    await translateWithAi([1], requestConfirmation);

    expect(startAiJob).toHaveBeenCalledWith(
      [1],
      "openai",
      [{ id: 1, targetText: "りんご", origin: "existing" }],
      { 1: { sample: "Sample Mod" } },
      "project-1",
      expect.any(Function),
    );
    expect(useAiJobStore.getState().activeJob).toMatchObject({ id: "job-1", total: 1 });
  });

  it("設定の取得中に入力した翻訳を最新件数で確認する", async () => {
    let finishSettings!: (settings: Awaited<ReturnType<typeof getAiSettings>>) => void;
    vi.mocked(getAiSettings).mockReturnValue(new Promise((resolve) => { finishSettings = resolve; }));
    useEditorStore.getState().loadEntries([{ ...translatedEntry, targetText: "", origin: "missing" }], "project-1");

    const action = translateWithAi([1], requestConfirmation);
    useEditorStore.getState().editEntry(1, "待機中に入力した訳");
    finishSettings({
      provider: "openai",
      configured: true,
      model: "example-model",
      baseUrl: "https://api.openai.com/v1",
      batchSize: 50,
      concurrency: 2,
      customPrompt: "",
    });
    await action;

    expect(requestConfirmation).toHaveBeenCalledWith({
      title: "既存の翻訳を上書きしますか？",
      message: "1件の翻訳が置き換わります。",
      confirmLabel: "上書き",
    });
    expect(startAiJob).not.toHaveBeenCalled();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("待機中に入力した訳");
  });

  it("ジョブ開始後に手入力した行へAI結果を適用しない", () => {
    useAiJobStore.getState().begin("openai", [1], 1, "project-1");
    useAiJobStore.getState().attachJobId("job-1");
    useEditorStore.getState().editEntry(1, "開始後に入力した訳");

    const event: AiJobEventDto = {
      kind: "batchResult",
      jobId: "job-1",
      entries: [{
        id: 1,
        modUnitId: 1,
        namespace: "sample",
        key: "item.apple",
        sourceText: "Apple",
        sourceLocale: "en_us",
        targetText: "AIの訳",
        targetOriginalText: null,
        translatedSourceText: "Apple",
        origin: "ai",
        sortIndex: 0,
        flags: [],
      }],
      rejected: [],
    };
    useAiJobStore.getState().receive(event);

    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("開始後に入力した訳");
    expect(useEditorStore.getState().entries.get(1)?.origin).toBe("edited");
  });

  it.each(["completed", "cancelled", "failed"] as const)("%sまでAI上書き前の手入力をUndoせず、終端後はAI結果をUndoできる", (kind) => {
    useEditorStore.getState().editEntry(1, "手入力", 1000);
    useEditorStore.getState().markSaved(useEditorStore.getState().getDirtyEntries());
    useAiJobStore.getState().begin("openai", [1], 1, "project-1");
    useAiJobStore.getState().receive(aiBatch());
    const beforeUndo = useEditorStore.getState();

    useEditorStore.getState().undo();
    useEditorStore.getState().redo();
    expect(useEditorStore.getState()).toBe(beforeUndo);
    expect(beforeUndo.entries.get(1)?.targetText).toBe("AIの訳");
    expect(beforeUndo.getDirtyEntries()).toEqual([]);

    useAiJobStore.getState().receive(kind === "failed"
      ? { kind, jobId: "job-1", error: { code: "ai_failed", userMessage: "失敗", recoverable: true } }
      : { kind, jobId: "job-1", completed: 1, total: 1 });
    expect(useEditorStore.getState().aiHistoryPending).toBe(false);
    useEditorStore.getState().undo();
    expect(useEditorStore.getState().getDirtyEntries()[0]?.targetText).toBe("手入力");
    useEditorStore.getState().redo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("AIの訳");
  });

  it("AI開始失敗で既存Redo履歴のロックを解除する", () => {
    useEditorStore.getState().editEntry(1, "手入力");
    useEditorStore.getState().undo();
    useAiJobStore.getState().begin("openai", [1], 1, "project-1");
    useEditorStore.getState().redo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("りんご");
    useAiJobStore.getState().failToStart("開始できませんでした");
    useEditorStore.getState().redo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("手入力");
  });

  it("AI実行中はツールバーのUndo/Redoを無効にする", () => {
    useEditorStore.getState().editEntry(1, "手入力");
    useEditorStore.getState().undo();
    render(createElement(EditorToolbar, {
      searchRef: { current: null }, onExport: vi.fn(), onToggleInspector: vi.fn(),
      inspectorOpen: false, onRetrySave: vi.fn(), saveStatus: "saved",
      requestConfirmation, onOpenGlossary: vi.fn(),
    }));
    expect((screen.getByRole("button", { name: "やり直す" }) as HTMLButtonElement).disabled).toBe(false);
    act(() => useAiJobStore.getState().begin("openai", [1], 1, "project-1"));
    expect((screen.getByRole("button", { name: "やり直す" }) as HTMLButtonElement).disabled).toBe(true);
    act(() => useAiJobStore.getState().receive(aiBatch()));
    expect((screen.getByRole("button", { name: "元に戻す" }) as HTMLButtonElement).disabled).toBe(true);
    act(() => useAiJobStore.getState().receive({ kind: "completed", jobId: "job-1", completed: 1, total: 1 }));
    expect((screen.getByRole("button", { name: "元に戻す" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("全体AI完了の再読込待ちに編集と保存が完了しても古いAI訳で上書きしない", async () => {
    const missing: TranslationEntry = { ...translatedEntry, modUnitId: 1, targetText: "", origin: "missing" };
    useEditorStore.getState().loadEntries([missing], "project-1");
    useEditorStore.getState().activateLoadedModUnit(1, "project-1");
    requestConfirmation.mockResolvedValue(true);
    let resolveRefresh!: (entries: TranslationEntry[]) => void;
    vi.mocked(listEntries).mockReset();
    vi.mocked(listEntries).mockResolvedValueOnce([missing]).mockResolvedValueOnce([missing])
      .mockImplementationOnce(() => new Promise((resolve) => { resolveRefresh = resolve; }));
    vi.mocked(listModUnits).mockResolvedValue([]);
    await translateWholeProjectWithAi(requestConfirmation);
    const onEvent = vi.mocked(startAiJob).mock.calls[0]?.[5];
    onEvent?.(aiBatch());
    const beforeUndo = useEditorStore.getState();
    useEditorStore.getState().undo();
    expect(useEditorStore.getState()).toBe(beforeUndo);
    onEvent?.({ kind: "completed", jobId: "job-1", completed: 1, total: 1 });
    expect(resolveRefresh).toBeTypeOf("function");
    useEditorStore.getState().editEntry(1, "完了待機中に保存した訳");
    useEditorStore.getState().markSaved(useEditorStore.getState().getDirtyEntries());
    expect(useEditorStore.getState().dirtyIds).toEqual([]);
    resolveRefresh([{ ...missing, targetText: "AIの訳", origin: "ai" }]);
    await waitFor(() => expect(useAiJobStore.getState().activeJob).toBeNull());
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("完了待機中に保存した訳");
    expect(useEditorStore.getState().saveStatus).toBe("saved");
    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("AIの訳");
    useEditorStore.getState().undo();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("");
  });

  it("準備中のキャンセル通知を受け取るとAI結果を反映しない", async () => {
    requestConfirmation.mockResolvedValue(true);
    await translateWithAi([1], requestConfirmation);
    const onEvent = vi.mocked(startAiJob).mock.calls[0]?.[5];
    expect(useAiJobStore.getState().activeJob?.id).toBe("job-1");

    onEvent?.({ kind: "cancelled", jobId: "job-1", completed: 0, total: 1 });

    expect(useAiJobStore.getState().activeJob).toBeNull();
    expect(useAiJobStore.getState().notice).toBe("AI翻訳をキャンセルしました。");
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("りんご");
  });
});
