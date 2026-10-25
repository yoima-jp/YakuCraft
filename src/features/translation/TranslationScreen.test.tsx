import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const translationScreenMocks = vi.hoisted(() => ({
  AppCommandError: class AppCommandError extends Error {
    constructor(readonly appError: { code: string; userMessage: string }) {
      super(appError.userMessage);
    }
  },
  getModUnitRemovalInfo: vi.fn(),
  getProjectSummary: vi.fn(),
  listEntries: vi.fn(),
  listGlossaryTerms: vi.fn(),
  listModUnits: vi.fn(),
  refreshModpackUnit: vi.fn(),
  removeModUnit: vi.fn(),
  updateEntriesBatch: vi.fn(),
  clearModIconCache: vi.fn(),
}));

vi.mock("../../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../lib/tauri")>()),
  ...translationScreenMocks,
}));

vi.mock("./aiTranslation", () => ({
  isUnitScopedAiJobActive: () => false,
  translateSelectedWithAi: vi.fn(),
  translateWholeProjectWithAi: vi.fn(),
  translateWithAi: vi.fn(),
}));

vi.mock("./ModTabs", async () => {
  const { createElement } = await import("react");
  return {
    clearModIconCacheInUi: vi.fn(),
    setModIconCacheProject: vi.fn(),
    ModTabs: (props: {
      activeUnitId: number;
      units: { id: number }[];
      selectionDisabled?: boolean;
      actionsDisabled?: boolean;
      onSelect: (id: number) => void;
      onRefreshModpack: (id: number) => void;
    }) => createElement("nav", {}, [
      ...props.units.map((unit) => createElement("button", {
        key: unit.id,
        type: "button",
        disabled: props.selectionDisabled,
        onClick: () => props.onSelect(unit.id),
      }, `Unit ${unit.id}`)),
      createElement("button", {
        key: "refresh",
        type: "button",
        disabled: props.actionsDisabled,
        onClick: () => props.onRefreshModpack(props.activeUnitId),
      }, "Modpack を再取り込み"),
    ]),
  };
});

vi.mock("./modTabCommands", () => ({ clearModIconCache: translationScreenMocks.clearModIconCache }));
vi.mock("./EditorToolbar", async () => {
  const { createElement } = await import("react");
  return {
    EditorToolbar: (props: { editingDisabled?: boolean }) => createElement(
      "button",
      { type: "button", disabled: props.editingDisabled },
      "元に戻す",
    ),
    EditorSelectionToolbar: () => null,
  };
});
vi.mock("./Inspector", () => ({ Inspector: () => null }));
vi.mock("./GlossaryPanel", () => ({ GlossaryPanel: () => null }));
vi.mock("./TranslationTable", () => ({ TranslationTable: () => null }));

import { useAppStore } from "../../stores/appStore";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useEditorStore } from "../../stores/editorStore";
import { useProjectStore } from "../../stores/projectStore";
import type { ProjectDetailsDto, ModUnitSummaryDto } from "../../types/project";
import type { TranslationEntry } from "../../types/translation";
import { TranslationScreen } from "./TranslationScreen";

const projectId = "refresh-project";

function details(): ProjectDetailsDto {
  return {
    project: {
      id: projectId,
      displayName: "fixture.zip",
      modCount: 2,
      instancePath: null,
      createdAt: "2026-01-01T00:00:00Z",
      updatedAt: "2026-01-01T00:00:00Z",
    },
    mods: [],
    namespaces: [],
    languages: [],
    nestedArchives: [],
    recommendedSourceLocale: "en_us",
    sourceLocale: "en_us",
    fallbackPriority: [],
    preserveExistingJapanese: true,
    fillMissingFromOtherLanguages: true,
    includedNamespaces: [],
    setupCompleted: true,
    existingJapaneseTranslationCount: 0,
    missingSourceKeysCount: 1,
    counts: { total: 1, translated: 0, missing: 1, needsReview: 0 },
    targetLanguageFileName: "ja_jp.json",
    targetLanguageFormat: "json",
    isSigned: false,
    assetCount: null,
  };
}

function modUnits(): ModUnitSummaryDto[] {
  return [
    {
      id: 1,
      sourceKind: "modpack",
      sourceFilename: "fixture.zip",
      modId: null,
      name: "Modpack",
      version: null,
      loader: null,
      minecraftVersion: null,
      total: 1,
      translated: 0,
      needsReview: 0,
    },
    {
      id: 2,
      sourceKind: "jar",
      sourceFilename: "other.jar",
      modId: "other",
      name: "Other",
      version: "1.0",
      loader: "Fabric",
      minecraftVersion: null,
      total: 1,
      translated: 0,
      needsReview: 0,
    },
  ];
}

function entry(targetText: string): TranslationEntry {
  return {
    id: 1,
    modUnitId: 1,
    namespace: "example",
    key: "example.key",
    sourceText: "English",
    sourceLocale: "en_us",
    targetText,
    origin: targetText ? "edited" : "missing",
    flags: [],
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  const units = modUnits();
  const originalEntries = [entry("")];
  translationScreenMocks.listModUnits.mockResolvedValue(units);
  translationScreenMocks.listEntries.mockResolvedValueOnce(originalEntries).mockResolvedValue([entry("Updated")]);
  translationScreenMocks.listGlossaryTerms.mockResolvedValue([]);
  translationScreenMocks.getProjectSummary.mockResolvedValue(details());
  translationScreenMocks.clearModIconCache.mockResolvedValue(undefined);
  useProjectStore.getState().setProjectDetails(details());
  useProjectStore.getState().setModUnits(units);
  useEditorStore.setState({
    entries: new Map(),
    orderedIds: [],
    hasLoadedEntries: false,
    loadedProjectId: null,
    activeModUnitId: null,
    unitStates: new Map(),
    deferredModAiUndoGroups: new Map(),
    dirtyIds: [],
    dirtyRevision: 0,
    saveStatus: "saved",
    saveError: null,
  });
  useAiJobStore.setState({
    activeJob: null,
    warnings: new Map(),
    notice: null,
    projectId: null,
    jobProjectId: null,
    jobProjectInvalidated: false,
    baseline: new Map(),
    pendingEdits: [],
    undoBoundary: [],
  });
  useAppStore.setState({ route: "editor" });
});

afterEach(cleanup);

describe("TranslationScreen Modpack refresh", () => {
  it("locks editing and tab selection while the refresh is running, then reloads the active unit", async () => {
    const refresh = deferred<void>();
    translationScreenMocks.refreshModpackUnit.mockReturnValue(refresh.promise);
    const { container } = render(<TranslationScreen />);

    fireEvent.click(await screen.findByRole("button", { name: "Modpack を再取り込み" }));
    expect(await screen.findByText("Modpack を再取り込み中…")).toBeTruthy();
    expect(container.querySelector(".editor-workspace")?.hasAttribute("inert")).toBe(true);
    expect((screen.getByRole("button", { name: "元に戻す" }) as HTMLButtonElement).disabled).toBe(true);
    const secondTab = screen.getByRole("button", { name: "Unit 2" }) as HTMLButtonElement;
    expect(secondTab.disabled).toBe(true);
    fireEvent.click(secondTab);
    expect(useEditorStore.getState().activeModUnitId).toBe(1);

    await act(async () => {
      refresh.resolve();
      await refresh.promise;
    });
    await waitFor(() => expect(screen.queryByText("Modpack を再取り込み中…")).toBeNull());
    expect(useEditorStore.getState().activeModUnitId).toBe(1);
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("Updated");
  });

  it("keeps another active unit and reloads refreshed entries the next time the unit is opened", async () => {
    const refresh = deferred<void>();
    translationScreenMocks.refreshModpackUnit.mockReturnValue(refresh.promise);
    render(<TranslationScreen />);

    fireEvent.click(await screen.findByRole("button", { name: "Modpack を再取り込み" }));
    await screen.findByText("Modpack を再取り込み中…");
    const otherEntry = { ...entry("Other unit"), id: 2, modUnitId: 2, namespace: "other" };
    act(() => useEditorStore.getState().activateModUnit(2, [otherEntry], projectId));

    await act(async () => {
      refresh.resolve();
      await refresh.promise;
    });
    await waitFor(() => expect(screen.queryByText("Modpack を再取り込み中…")).toBeNull());
    expect(useEditorStore.getState().activeModUnitId).toBe(2);
    expect(useEditorStore.getState().entries.get(2)?.targetText).toBe("Other unit");
    expect(useEditorStore.getState().unitStates.has(1)).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Unit 1" }));
    await waitFor(() => expect(useEditorStore.getState().activeModUnitId).toBe(1));
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("Updated");
  });
});
