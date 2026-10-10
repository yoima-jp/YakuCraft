import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { openFileDialog } from "../../lib/dialogs";
import { createProjectFromJars, getProjectSummary, listModUnits, listRecentProjects } from "../../lib/tauri";
import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import type { AnalysisChannelEvent } from "../../types/commands";
import type { ProjectDetailsDto } from "../../types/project";
import { HomeScreen } from "./HomeScreen";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false }));
vi.mock("../../lib/dialogs", () => ({ openFileDialog: vi.fn() }));
vi.mock("../updater/UpdaterNotice", () => ({ UpdaterNotice: () => null }));
vi.mock("../../lib/tauri", () => ({
  AppCommandError: class AppCommandError extends Error {},
  cancelAnalysis: vi.fn().mockResolvedValue(undefined),
  createProjectFromJars: vi.fn(),
  deleteProject: vi.fn(),
  getProjectSummary: vi.fn(),
  listEntries: vi.fn(),
  listModUnits: vi.fn(),
  listRecentProjects: vi.fn(),
  openProject: vi.fn(),
  renameProject: vi.fn(),
}));

const details: ProjectDetailsDto = {
  project: {
    id: "created-project", displayName: "fixture.jar", modCount: 1, instancePath: null,
    createdAt: "2026-01-01T00:00:00Z", updatedAt: "2026-01-01T00:00:00Z",
  },
  mods: [], namespaces: [], languages: [], nestedArchives: [],
  recommendedSourceLocale: "en_us", sourceLocale: "en_us", fallbackPriority: [],
  preserveExistingJapanese: true, fillMissingFromOtherLanguages: true,
  includedNamespaces: [], setupCompleted: false,
  existingJapaneseTranslationCount: 0, missingSourceKeysCount: 0,
  counts: { total: 0, translated: 0, missing: 0, needsReview: 0 },
  targetLanguageFileName: "ja_jp.json", targetLanguageFormat: "json", isSigned: false, assetCount: 0,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

let onAnalysisEvent: (event: AnalysisChannelEvent) => void;

beforeEach(() => {
  vi.clearAllMocks();
  useAppStore.setState({ route: "home", settingsReturnRoute: "home" });
  useProjectStore.setState(useProjectStore.getInitialState());
  vi.mocked(openFileDialog).mockResolvedValue("C:/mods/fixture.jar");
  vi.mocked(listRecentProjects).mockResolvedValue([]);
  vi.mocked(getProjectSummary).mockResolvedValue(details);
  vi.mocked(listModUnits).mockResolvedValue([]);
  vi.mocked(createProjectFromJars).mockImplementation(async (_paths, _instance, callback) => {
    onAnalysisEvent = callback;
    return "analysis-job";
  });
});

afterEach(cleanup);

async function startAnalysis() {
  const home = render(<HomeScreen />);
  fireEvent.click(screen.getByTestId("home-open-jar"));
  await waitFor(() => expect(useProjectStore.getState().analysisJobId).toBe("analysis-job"));
  expect(useAppStore.getState().route).toBe("analyze");
  // 実際のAppと同様に、解析開始時にHomeをアンマウントしてcallbackだけを残す。
  home.unmount();
}

function completeAnalysis() {
  onAnalysisEvent({
    kind: "completed",
    completed: { jobId: "analysis-job", projectId: details.project.id, failedMods: [] },
  });
}

describe("Home analysis completion navigation", () => {
  it("解析画面にいる場合は結果を反映してreviewへ進む", async () => {
    await startAnalysis();
    await act(async () => { completeAnalysis(); });
    expect(useAppStore.getState().route).toBe("review");
    expect(useProjectStore.getState().details).toEqual(details);
    expect(useProjectStore.getState().analysisJobId).toBeNull();
  });

  it.each(["home", "settings"] as const)("完了通知前に%sへ移動しても画面を維持する", async (route) => {
    await startAnalysis();
    await act(async () => { await useAppStore.getState().goTo("home"); });
    render(<HomeScreen />);
    if (route === "settings") {
      await act(async () => { await useAppStore.getState().openSettings(); });
    }
    await act(async () => { completeAnalysis(); });
    expect(useAppStore.getState().route).toBe(route);
    expect(useProjectStore.getState().details).toEqual(details);
    expect(useProjectStore.getState().analysisJobId).toBeNull();
  });

  it("Homeに残ったまま完了したProjectを最近の一覧に表示する", async () => {
    await startAnalysis();
    await act(async () => { await useAppStore.getState().goTo("home"); });
    render(<HomeScreen />);
    const recentProject = {
      id: details.project.id,
      displayName: details.project.displayName,
      modVersion: null,
      modCount: 1,
      instancePath: null,
      lastOpened: details.project.updatedAt,
      translationProgress: { translated: 0, missing: 1, needsReview: 0 },
    };
    vi.mocked(listRecentProjects).mockResolvedValue([recentProject]);
    await act(async () => { completeAnalysis(); });
    expect(await screen.findByText("fixture.jar")).toBeTruthy();
    expect(useProjectStore.getState().recentProjects).toEqual([recentProject]);
    expect(useAppStore.getState().route).toBe("home");
  });

  it.each(["summary", "modUnits"] as const)("%s取得待ちの間にHomeと設定へ移動しても設定を閉じない", async (stage) => {
    const summary = deferred<ProjectDetailsDto>();
    const units = deferred<Awaited<ReturnType<typeof listModUnits>>>();
    if (stage === "summary") vi.mocked(getProjectSummary).mockReturnValue(summary.promise);
    else vi.mocked(listModUnits).mockReturnValue(units.promise);
    await startAnalysis();
    await act(async () => { completeAnalysis(); });
    expect(useAppStore.getState().route).toBe("analyze");
    await act(async () => {
      await useAppStore.getState().goTo("home");
      await useAppStore.getState().openSettings();
    });
    await act(async () => {
      if (stage === "summary") summary.resolve(details);
      else units.resolve([]);
    });
    expect(useAppStore.getState().route).toBe("settings");
    expect(useProjectStore.getState().details).toEqual(details);
  });
});
