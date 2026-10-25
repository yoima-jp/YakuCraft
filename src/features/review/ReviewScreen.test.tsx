import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import type { ProjectDetailsDto } from "../../types/project";
import { getReviewAssetSummary, listEntries, saveReviewSettings } from "../../lib/tauri";
import { ReviewScreen } from "./ReviewScreen";

vi.mock("../../lib/tauri", () => ({
  AppCommandError: class AppCommandError extends Error {
    appError: { code: string; userMessage: string; recoverable: boolean };
    constructor(appError: { code: string; userMessage: string; recoverable: boolean }) {
      super(appError.userMessage);
      this.appError = appError;
    }
  },
  getReviewAssetSummary: vi.fn(),
  listEntries: vi.fn(),
  saveReviewSettings: vi.fn(),
}));

function details(): ProjectDetailsDto {
  return {
    project: {
      id: "project-1",
      displayName: "fixture.jar",
      modCount: 1,
      instancePath: null,
      createdAt: "2026-01-01T00:00:00Z",
      updatedAt: "2026-01-01T00:00:00Z",
    },
    mods: [{ modUnitId: 1, modId: "fixture", name: "Fixture", version: "1.0", loader: "Fabric", minecraftVersion: "1.21.1" }],
    namespaces: [
      { modUnitId: 1, namespace: "fixture", containerPath: "", included: true, entryCount: 2 },
      { modUnitId: 1, namespace: "library", containerPath: "META-INF/jars/library.jar", included: false, entryCount: 1 },
    ],
    languages: [
      { modUnitId: 1, namespace: "fixture", locale: "en_us", format: "json", entryCount: 2, containerPath: "", archivePath: "assets/fixture/lang/en_us.json" },
      { modUnitId: 1, namespace: "fixture", locale: "ja_jp", format: "json", entryCount: 1, containerPath: "", archivePath: "assets/fixture/lang/ja_jp.json" },
      { modUnitId: 1, namespace: "library", locale: "en_us", format: "json", entryCount: 1, containerPath: "META-INF/jars/library.jar", archivePath: "assets/library/lang/en_us.json" },
    ],
    nestedArchives: [{ modUnitId: 1, archivePath: "META-INF/jars/library.jar", modId: "library", name: "Library", version: "1", hasLanguages: true, skippedReason: null }],
    recommendedSourceLocale: "en_us",
    sourceLocale: "en_us",
    fallbackPriority: [],
    preserveExistingJapanese: true,
    fillMissingFromOtherLanguages: true,
    includedNamespaces: [{ modUnitId: 1, namespace: "fixture" }],
    setupCompleted: false,
    existingJapaneseTranslationCount: 1,
    missingSourceKeysCount: 1,
    counts: { total: 2, translated: 1, missing: 1, needsReview: 0 },
    targetLanguageFileName: "ja_jp.json",
    targetLanguageFormat: "json",
    isSigned: false,
    assetCount: 6,
    minecraftVersionDetection: {
      version: "1.21.1",
      method: "metadata",
      originalValue: ">=1.21",
      rounded: false,
    },
    supportedMinecraftVersions: ["1.21", "1.21.1", "1.21.2"],
    assetSummary: {
      total: 6,
      blockModels: 2,
      itemModels: 1,
      entityTextures: 1,
      geometryModels: 1,
      unresolvedEntries: 1,
    },
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  useAppStore.setState({ route: "review" });
  useProjectStore.getState().setModUnits([{
    id: 1,
    sourceFilename: "fixture.jar",
    modId: "fixture",
    name: "Fixture",
    version: "1.0",
    loader: "Fabric",
    minecraftVersion: "1.21.1",
    total: 2,
    translated: 1,
    needsReview: 0,
  }]);
  useProjectStore.getState().setProjectDetails(details());
  vi.mocked(saveReviewSettings).mockResolvedValue(details());
  vi.mocked(listEntries).mockResolvedValue([]);
  vi.mocked(getReviewAssetSummary).mockResolvedValue({
    existingJapaneseTranslationCount: 1,
    missingSourceKeysCount: 1,
    assets: details().assetSummary!,
  });
});

afterEach(() => cleanup());

describe("Review setup flow", () => {
  it("5つのステップを進み、明示した選択を保存してEditorへ進む", async () => {
    render(<ReviewScreen />);
    expect(screen.getByText("1 / 5")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "翻訳対象に含める" }));
    expect(screen.getByRole("checkbox", { name: "library · 1" })).toHaveProperty("checked", false);
    fireEvent.click(screen.getByRole("checkbox", { name: "library · 1" }));

    await waitFor(() => expect(screen.getByTestId("review-next")).toHaveProperty("disabled", false));
    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    expect(screen.getByText("2 / 5")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    expect(screen.getByText("3 / 5")).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: "すべて新しく翻訳する" }));
    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    expect(screen.getByText("4 / 5")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    expect(await screen.findByText("5 / 5")).toBeTruthy();
    expect(screen.getByText("プレビューに使えるアセットを 6 件見つけました")).toBeTruthy();
    expect(getReviewAssetSummary).toHaveBeenCalledWith(expect.objectContaining({
      includedNamespaces: [{ modUnitId: 1, namespace: "fixture" }, { modUnitId: 1, namespace: "library" }],
      preserveExistingJapanese: true,
    }), expect.any(Function));
    expect(screen.queryByText("プレビューは準備中です")).toBeNull();
    expect(screen.getByText("ブロックモデル")).toBeTruthy();
    expect(screen.getByText("未解決の項目")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "翻訳を始める" }));
    await waitFor(() => expect(saveReviewSettings).toHaveBeenCalledWith(expect.objectContaining({
      sourceLocale: "en_us",
      preserveExistingJapanese: false,
      includedNamespaces: [{ modUnitId: 1, namespace: "fixture" }, { modUnitId: 1, namespace: "library" }],
    }), expect.any(Function)));
    await waitFor(() => expect(useAppStore.getState().route).toBe("editor"));
  });

  it("0件の段階を省き、既定値を保存する", async () => {
    const emptyDetails = details();
    emptyDetails.languages = emptyDetails.languages.filter((language) => language.locale === "en_us");
    emptyDetails.existingJapaneseTranslationCount = 0;
    emptyDetails.missingSourceKeysCount = 0;
    emptyDetails.assetCount = 0;
    emptyDetails.assetSummary = {
      total: 0,
      blockModels: 0,
      itemModels: 0,
      entityTextures: 0,
      geometryModels: 0,
      unresolvedEntries: 0,
    };
    useProjectStore.getState().setProjectDetails(emptyDetails);

    render(<ReviewScreen />);
    expect(screen.getByText("1 / 2")).toBeTruthy();
    expect(screen.queryByText("既存の日本語訳を引き継ぎますか？")).toBeNull();
    expect(screen.queryByText("不足キーを補完しますか？")).toBeNull();
    expect(screen.queryByText("プレビュー用アセット")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    expect(screen.getByText("2 / 2")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "変更" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "翻訳を始める" }));

    await waitFor(() => expect(saveReviewSettings).toHaveBeenCalledWith(expect.objectContaining({
      preserveExistingJapanese: true,
      fillMissingFromOtherLanguages: true,
    }), expect.any(Function)));
  });

  it("戻る操作でも0件で省いた段階に入らない", () => {
    const partialDetails = details();
    partialDetails.existingJapaneseTranslationCount = 0;
    partialDetails.missingSourceKeysCount = 2;
    partialDetails.assetCount = 0;
    partialDetails.assetSummary = { ...partialDetails.assetSummary!, total: 0 };
    useProjectStore.getState().setProjectDetails(partialDetails);

    render(<ReviewScreen />);
    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    expect(screen.getByText("3 / 3")).toBeTruthy();
    expect(screen.getByText("不足キーを補完しますか？")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "戻る" }));
    expect(screen.getByText("2 / 3")).toBeTruthy();
    expect(screen.getByText("翻訳元を確認")).toBeTruthy();
    expect(screen.queryByText("既存の日本語訳を引き継ぎますか？")).toBeNull();
  });

  it("Namespaceを変えた後の件数から表示段階を決める", async () => {
    vi.mocked(getReviewAssetSummary).mockResolvedValue({
      existingJapaneseTranslationCount: 0,
      missingSourceKeysCount: 0,
      assets: {
        total: 0,
        blockModels: 0,
        itemModels: 0,
        entityTextures: 0,
        geometryModels: 0,
        unresolvedEntries: 0,
      },
    });
    render(<ReviewScreen />);
    fireEvent.click(screen.getByRole("button", { name: "翻訳対象に含める" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "library · 1" }));

    expect(await screen.findByText("1 / 2")).toBeTruthy();
    expect(screen.queryByText("既存の日本語訳を引き継ぎますか？")).toBeNull();
    expect(screen.queryByText("不足キーを補完しますか？")).toBeNull();
    expect(screen.queryByText("プレビュー用アセット")).toBeNull();
  });

  it("Patchouli本文だけの同梱名前空間を選択して保存する", async () => {
    const patchouliDetails = details();
    patchouliDetails.namespaces = [
      patchouliDetails.namespaces[0]!,
      { modUnitId: 1, namespace: "guidebook", containerPath: "META-INF/jars/guidebook.jar", included: false, entryCount: 1 },
    ];
    patchouliDetails.languages = patchouliDetails.languages.filter((language) => language.namespace !== "library");
    patchouliDetails.nestedArchives = [{
      modUnitId: 1,
      archivePath: "META-INF/jars/guidebook.jar",
      modId: "guidebook",
      name: "Guidebook",
      version: "1",
      hasLanguages: false,
      skippedReason: null,
    }];
    patchouliDetails.includedNamespaces = [{ modUnitId: 1, namespace: "fixture" }];
    useProjectStore.getState().setProjectDetails(patchouliDetails);
    vi.mocked(getReviewAssetSummary).mockResolvedValue({
      existingJapaneseTranslationCount: 0,
      missingSourceKeysCount: 0,
      assets: {
        total: 0,
        blockModels: 0,
        itemModels: 0,
        entityTextures: 0,
        geometryModels: 0,
        unresolvedEntries: 0,
      },
    });

    render(<ReviewScreen />);
    expect(screen.getByText("同梱Mod 1件")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "翻訳対象に含める" }));
    const namespaceCheckbox = screen.getByRole("checkbox", { name: "guidebook · 1" });
    expect(namespaceCheckbox).toHaveProperty("checked", false);
    fireEvent.click(namespaceCheckbox);
    expect(namespaceCheckbox).toHaveProperty("checked", true);

    await waitFor(() => expect(getReviewAssetSummary).toHaveBeenCalledWith(expect.objectContaining({
      includedNamespaces: [{ modUnitId: 1, namespace: "fixture" }, { modUnitId: 1, namespace: "guidebook" }],
    }), expect.any(Function)));
    await waitFor(() => expect(screen.getByTestId("review-next")).toHaveProperty("disabled", false));
    fireEvent.click(screen.getByRole("button", { name: "次へ" }));
    fireEvent.click(screen.getByRole("button", { name: "翻訳を始める" }));

    await waitFor(() => expect(saveReviewSettings).toHaveBeenCalledWith(expect.objectContaining({
      includedNamespaces: [{ modUnitId: 1, namespace: "fixture" }, { modUnitId: 1, namespace: "guidebook" }],
    }), expect.any(Function)));
    await waitFor(() => expect(useAppStore.getState().route).toBe("editor"));
  });
});
