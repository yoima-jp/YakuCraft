import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getProjectSummary, listModUnits } from "../../lib/tauri";
import { openFileDialog, saveFileDialog } from "../../lib/dialogs";
import { useProjectStore } from "../../stores/projectStore";
import type { ProjectDetailsDto } from "../../types/project";
import { ExportScreen } from "./ExportScreen";
import {
  exportSelectedMergedResourcePack,
  exportSelectedModifiedJars,
  exportShaderpacks,
  getExportOverview,
  restoreModifiedJar,
  validateSelectedProject,
} from "./exportApi";

vi.mock("../../lib/tauri", () => ({
  AppCommandError: class AppCommandError extends Error {
    constructor(appError: { userMessage: string }) {
      super(appError.userMessage);
    }
  },
  exportLanguageFile: vi.fn(),
  exportModifiedJar: vi.fn(),
  exportResourcePack: vi.fn(),
  getProjectSummary: vi.fn(),
  listModUnits: vi.fn(),
  openOutputFolder: vi.fn(),
  validateProject: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn() }));
vi.mock("../../lib/dialogs", () => ({ openFileDialog: vi.fn(), saveFileDialog: vi.fn() }));

vi.mock("./exportApi", () => ({
  ExportCommandError: class ExportCommandError extends Error {},
  exportSelectedLanguageFile: vi.fn(),
  exportSelectedLanguageFiles: vi.fn(),
  exportSelectedMergedResourcePack: vi.fn(),
  exportSelectedModifiedJars: vi.fn(),
  exportShaderpacks: vi.fn(),
  getExportOverview: vi.fn(),
  restoreModifiedJar: vi.fn(),
  validateSelectedProject: vi.fn(),
}));

function details(projectId = "project-1"): ProjectDetailsDto {
  return {
    project: {
      id: projectId,
      displayName: projectId === "project-1" ? "fixture.jar" : "other.jar",
      modCount: 1,
      instancePath: null,
      createdAt: "2026-01-01T00:00:00Z",
      updatedAt: "2026-01-01T00:00:00Z",
    },
    mods: [{ modUnitId: 1, modId: "fixture", name: "Fixture", version: "1.0", loader: "Fabric", minecraftVersion: null }],
    namespaces: [{ modUnitId: 1, namespace: "fixture", containerPath: "", included: true, entryCount: 2 }],
    languages: [{ modUnitId: 1, namespace: "fixture", locale: "en_us", format: "json", entryCount: 2, containerPath: "", archivePath: "assets/fixture/lang/en_us.json" }],
    nestedArchives: [],
    recommendedSourceLocale: "en_us",
    sourceLocale: "en_us",
    fallbackPriority: [],
    preserveExistingJapanese: true,
    fillMissingFromOtherLanguages: true,
    includedNamespaces: [{ modUnitId: 1, namespace: "fixture" }],
    setupCompleted: true,
    existingJapaneseTranslationCount: 0,
    missingSourceKeysCount: 0,
    counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
    targetLanguageFileName: "ja_jp.json",
    targetLanguageFormat: "json",
    isSigned: false,
    assetCount: null,
    minecraftVersionDetection: {
      version: "1.21.1",
      method: "metadata",
      originalValue: ">=1.21",
      rounded: false,
    },
    supportedMinecraftVersions: ["1.21", "1.21.1", "1.21.2"],
  };
}

function modUnit(id: number) {
  return {
    id,
    sourceFilename: `fixture-${id}.jar`,
    modId: `fixture-${id}`,
    name: `Fixture ${id}`,
    version: "1.0",
    loader: "Fabric",
    minecraftVersion: null,
    total: 2,
    translated: 1,
    needsReview: 1,
  };
}

function setupInstanceWithShaderpacks() {
  const units = [modUnit(1), modUnit(2), modUnit(3)].map((unit, index) => (
    index === 0 ? { ...unit, sourceKind: "jar" as const } : { ...unit, sourceKind: "shaderpack" as const }
  ));
  useProjectStore.getState().setModUnits(units);
  vi.mocked(listModUnits).mockResolvedValue(units);
  vi.mocked(getExportOverview).mockResolvedValue({
    instancePath: "C:\\instance",
    units: units.map((unit) => ({
      id: unit.id,
      sourceKind: unit.sourceKind,
      sourceFilename: unit.sourceFilename,
      name: unit.name,
      version: unit.version,
      sourceSha256: "source-hash",
      originalPath: `C:\\mods\\${unit.sourceFilename}`,
      originalExists: true,
    })),
    activeReplacements: [],
  });
}

function setupInstanceWithSingleShaderpack() {
  const units = [modUnit(1), modUnit(2), modUnit(3)].map((unit, index) => (
    index === 2 ? { ...unit, sourceKind: "shaderpack" as const } : { ...unit, sourceKind: "jar" as const }
  ));
  useProjectStore.getState().setModUnits(units);
  vi.mocked(listModUnits).mockResolvedValue(units);
  vi.mocked(getExportOverview).mockResolvedValue({
    instancePath: "C:\\instance",
    units: units.map((unit) => ({
      id: unit.id,
      sourceKind: unit.sourceKind,
      sourceFilename: unit.sourceFilename,
      name: unit.name,
      version: unit.version,
      sourceSha256: "source-hash",
      originalPath: `C:\\mods\\${unit.sourceFilename}`,
      originalExists: true,
    })),
    activeReplacements: [],
  });
}

function setupInstanceWithModOnly() {
  const units = [{ ...modUnit(1), sourceKind: "jar" as const }];
  useProjectStore.getState().setModUnits(units);
  vi.mocked(listModUnits).mockResolvedValue(units);
  vi.mocked(getExportOverview).mockResolvedValue({
    instancePath: "C:\\instance",
    units: units.map((unit) => ({
      id: unit.id,
      sourceKind: unit.sourceKind,
      sourceFilename: unit.sourceFilename,
      name: unit.name,
      version: unit.version,
      sourceSha256: "source-hash",
      originalPath: `C:\\mods\\${unit.sourceFilename}`,
      originalExists: true,
    })),
    activeReplacements: [],
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

beforeEach(() => {
  vi.clearAllMocks();
  useProjectStore.getState().setProjectDetails(details());
  useProjectStore.getState().setModUnits([modUnit(1)]);
  vi.mocked(getProjectSummary).mockResolvedValue(details());
  vi.mocked(listModUnits).mockResolvedValue(useProjectStore.getState().modUnits);
  vi.mocked(getExportOverview).mockResolvedValue({
    instancePath: null,
    units: [{
      id: 1,
      sourceFilename: "fixture.jar",
      name: "Fixture",
      version: "1.0",
      sourceSha256: "source-hash",
      originalPath: "C:\\mods\\fixture.jar",
      originalExists: true,
    }],
    activeReplacements: [],
  });
  vi.mocked(saveFileDialog).mockResolvedValue(null);
  vi.mocked(openFileDialog).mockResolvedValue(null);
  vi.mocked(validateSelectedProject).mockImplementation(async () => ({
    issues: [{
      severity: "warning",
      code: "untranslatedEntries",
      message: "1 translation entries are empty",
      modUnitId: 1,
      modName: "Fixture 1.0",
      count: 1,
      namespace: null,
      key: null,
      sourceText: null,
      targetText: null,
    }],
    minecraftVersionDetection: details().minecraftVersionDetection ?? null,
    counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
    isSigned: false,
  }));
});

afterEach(() => cleanup());

describe("Export format and validation", () => {
  it("形式切り替えで唯一のシェーダーパックを選択する", async () => {
    setupInstanceWithSingleShaderpack();
    render(<ExportScreen />);

    fireEvent.click(await screen.findByRole("button", { name: "全選択" }));
    fireEvent.click(await screen.findByRole("radio", { name: /日本語化したシェーダーパック/ }));

    await waitFor(() => expect(validateSelectedProject).toHaveBeenLastCalledWith(
      "project-1",
      "shaderpackZip",
      [3],
      undefined,
      undefined,
      undefined,
    ));
  });

  it("リソースパック単体のインスタンス配置では事前確認を出さず上書き確認を保つ", async () => {
    setupInstanceWithModOnly();
    vi.mocked(exportSelectedMergedResourcePack)
      .mockResolvedValueOnce({ status: "overwriteRequired", outputPath: "C:\\instance\\resourcepacks\\fixture.zip" })
      .mockResolvedValue({ status: "completed", outputPath: "C:\\instance\\resourcepacks\\fixture.zip", directlyInstalled: true, modResults: [], conflicts: [] });
    render(<ExportScreen />);

    fireEvent.click(await screen.findByTestId("export-start"));
    const overwriteDialog = await screen.findByRole("dialog", { name: "リソースパックを上書きしますか" });
    expect(overwriteDialog.textContent).toContain("同じ名前のリソースパックがあります。");
    fireEvent.click(screen.getByRole("button", { name: "上書き" }));

    await waitFor(() => expect(exportSelectedMergedResourcePack).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("dialog", { name: "インスタンスに配置しますか" })).toBeNull();
  });

  it("詳細設定は初期状態で閉じ、押すと開閉する", async () => {
    render(<ExportScreen />);
    const disclosure = await screen.findByRole("button", { name: "詳細設定" });

    expect(disclosure.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("textbox", { name: "表にない Minecraft バージョン" })).toBeNull();

    fireEvent.click(disclosure);
    expect(disclosure.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("textbox", { name: "表にない Minecraft バージョン" })).toBeTruthy();

    fireEvent.click(disclosure);
    expect(disclosure.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("textbox", { name: "表にない Minecraft バージョン" })).toBeNull();
  });

  it("format selection revalidates and displays returned warnings", async () => {
    render(<ExportScreen />);
    const modifiedJar = await screen.findByRole("radio", { name: /Mod JARに埋め込む/ });
    fireEvent.click(modifiedJar);

    await waitFor(() => expect(validateSelectedProject).toHaveBeenCalledWith("project-1", "modifiedJar", [1], undefined, undefined, undefined));
    const summary = await screen.findByText("未翻訳 合計 1 件");
    expect(summary).toBeTruthy();
    fireEvent.click(summary);
    await waitFor(() => expect(screen.getByRole("list", { name: "modごとの未翻訳件数" })).toBeTruthy());
    expect(screen.getByText("1 / 2")).toBeTruthy();
  });

  it("placeholder のエラーと注意を集約して表示する", async () => {
    vi.mocked(validateSelectedProject).mockResolvedValue({
      issues: [
        {
          severity: "error",
          code: "placeholderError",
          message: "Placeholderの書式エラー: fixture:broken.format",
          modUnitId: 1,
          modName: "Fixture 1.0",
          count: null,
          namespace: "fixture",
          key: "broken.format",
          sourceText: "%s",
          targetText: "%2$s",
        },
        {
          severity: "warning",
          code: "placeholderWarning",
          message: "引数が表示されない可能性があります: fixture:missing.argument",
          modUnitId: 1,
          modName: "Fixture 1.0",
          count: null,
          namespace: "fixture",
          key: "missing.argument",
          sourceText: "%s %s",
          targetText: "%s",
        },
      ],
      minecraftVersionDetection: details().minecraftVersionDetection ?? null,
      counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
      isSigned: false,
    });

    render(<ExportScreen />);
    const summary = await screen.findByText("書式エラー 1 件・注意 1 件");
    fireEvent.click(summary);

    expect(await screen.findByRole("list", { name: "書式の確認事項" })).toBeTruthy();
  });

  it("書式エラーで書き出しが止まると詳細を開いて理由を表示する", async () => {
    vi.mocked(validateSelectedProject).mockResolvedValue({
      issues: [{ severity: "error", code: "placeholderError", message: "書式エラー", modUnitId: 1, modName: "Fixture", count: null, namespace: "fixture", key: "broken", sourceText: "%s", targetText: "%2$s" }],
      minecraftVersionDetection: details().minecraftVersionDetection ?? null,
      counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
      isSigned: false,
    });
    render(<ExportScreen />);
    const summary = await screen.findByText("書式エラー 1 件・注意 0 件");
    fireEvent.click(screen.getByTestId("export-start"));
    const dialog = await screen.findByRole("dialog", { name: "書式エラーがあります" });
    expect(dialog.textContent).toContain("1 件の訳はゲーム内で正しく表示されないことがあります。");
    fireEvent.click(dialog.querySelector("button")!);
    expect(summary.closest("details")?.open).toBe(true);
  });

  it("書式エラーだけならこのまま書き出せる", async () => {
    vi.mocked(validateSelectedProject).mockResolvedValue({
      issues: [{ severity: "error", code: "placeholderError", message: "書式エラー", modUnitId: 1, modName: "Fixture", count: null, namespace: "fixture", key: "broken", sourceText: "%s", targetText: "%2$s" }],
      minecraftVersionDetection: details().minecraftVersionDetection ?? null,
      counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
      isSigned: false,
    });
    vi.mocked(saveFileDialog).mockResolvedValue("output.zip");
    vi.mocked(exportSelectedMergedResourcePack).mockResolvedValue({ status: "completed", outputPath: "output.zip", directlyInstalled: false, modResults: [], conflicts: [] });
    render(<ExportScreen />);
    await screen.findByText("書式エラー 1 件・注意 0 件");
    fireEvent.click(screen.getByTestId("export-start"));
    fireEvent.click(await screen.findByRole("button", { name: "このまま書き出す" }));
    await waitFor(() => expect(exportSelectedMergedResourcePack).toHaveBeenCalled());
  });

  it("Mod JARでも書式エラーを確認し、ほかのエラーがあってもJAR書き出しを続ける", async () => {
    vi.mocked(validateSelectedProject).mockResolvedValue({
      issues: [
        { severity: "error", code: "placeholderError", message: "書式エラー", modUnitId: 1, modName: "Fixture", count: null, namespace: "fixture", key: "broken", sourceText: "%s", targetText: "%2$s" },
        { severity: "error", code: "parseError", message: "解析失敗", modUnitId: 1, modName: "Fixture", count: null, namespace: null, key: null, sourceText: null, targetText: null },
      ],
      minecraftVersionDetection: details().minecraftVersionDetection ?? null,
      counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
      isSigned: false,
    });
    vi.mocked(openFileDialog).mockResolvedValue("C:\\exports");
    vi.mocked(exportSelectedModifiedJars).mockResolvedValue([{
      modUnitId: 1,
      modName: "Fixture",
      status: "success",
      outputPath: "C:\\exports\\fixture-ja.jar",
      message: null,
    }]);
    render(<ExportScreen />);
    fireEvent.click(await screen.findByRole("radio", { name: /Mod JARに埋め込む/ }));
    fireEvent.click(await screen.findByTestId("export-start"));

    const dialog = await screen.findByRole("dialog", { name: "書式エラーがあります" });
    expect(dialog.textContent).toContain("1 件の訳はゲーム内で正しく表示されないことがあります。");
    expect(exportSelectedModifiedJars).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "このまま書き出す" }));

    await waitFor(() => expect(exportSelectedModifiedJars).toHaveBeenCalledOnce());
    expect(screen.queryByText("確認事項を直すと書き出せます")).toBeNull();
  });

  it("同時配置するシェーダーパックの書式エラーも強制確認を出す", async () => {
    setupInstanceWithShaderpacks();
    vi.mocked(validateSelectedProject).mockImplementation(async (_projectId, kind) => ({
      issues: kind === "shaderpackZip"
        ? [{ severity: "error", code: "placeholderError", message: "シェーダーの書式エラー", modUnitId: 2, modName: "Fixture 2", count: null, namespace: "shader", key: "broken", sourceText: "%s", targetText: "%2$s" }]
        : [],
      minecraftVersionDetection: details().minecraftVersionDetection ?? null,
      counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
      isSigned: false,
    }));
    render(<ExportScreen />);
    fireEvent.click(await screen.findByTestId("export-start"));

    expect(await screen.findByRole("dialog", { name: "書式エラーがあります" })).toBeTruthy();
    expect(exportSelectedMergedResourcePack).not.toHaveBeenCalled();
    expect(exportShaderpacks).not.toHaveBeenCalled();
  });

  it("Mod JARとシェーダーパック同時配置の確認に配置先を示し、上書き対象を列挙する", async () => {
    setupInstanceWithShaderpacks();
    vi.mocked(openFileDialog).mockResolvedValue("C:\\exports");
    vi.mocked(exportSelectedModifiedJars).mockResolvedValue([{
      modUnitId: 1,
      modName: "Fixture",
      status: "success",
      outputPath: "C:\\mods\\fixture-1-ja.jar",
      message: null,
    }]);
    vi.mocked(exportShaderpacks).mockResolvedValue({
      status: "overwriteRequired",
      targetPaths: ["C:\\instance\\shaderpacks\\first.zip", "C:\\instance\\shaderpacks\\second.zip"],
    });
    render(<ExportScreen />);
    fireEvent.click(await screen.findByRole("radio", { name: /Mod JARに埋め込む/ }));
    fireEvent.click(await screen.findByRole("checkbox", { name: "mods フォルダの JAR を直接上書きする" }));
    fireEvent.click(await screen.findByRole("button", { name: "置き換え" }));
    expect(await screen.findByRole("checkbox", { name: "シェーダーパックも shaderpacks/ に配置する" })).toBeTruthy();
    fireEvent.click(await screen.findByTestId("export-start"));

    const deploymentDialog = await screen.findByRole("dialog", { name: "インスタンスに配置しますか" });
    expect(deploymentDialog.textContent).toContain("shaderpacks/");
    expect(deploymentDialog.textContent).toContain("既存のシェーダーパックはバックアップされ");
    expect(exportSelectedModifiedJars).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "配置" }));

    const overwriteDialog = await screen.findByRole("dialog", { name: "シェーダーパックを上書きしますか" });
    expect(overwriteDialog.textContent).toContain("first.zip");
    expect(overwriteDialog.textContent).toContain("second.zip");
  });

  it("書式以外のエラーがあると強制書き出しを表示しない", async () => {
    vi.mocked(validateSelectedProject).mockResolvedValue({
      issues: [
        { severity: "error", code: "placeholderError", message: "書式エラー", modUnitId: 1, modName: "Fixture", count: null, namespace: "fixture", key: "broken", sourceText: "%s", targetText: "%2$s" },
        { severity: "error", code: "parseError", message: "解析失敗", modUnitId: 1, modName: "Fixture", count: null, namespace: null, key: null, sourceText: null, targetText: null },
      ],
      minecraftVersionDetection: details().minecraftVersionDetection ?? null,
      counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
      isSigned: false,
    });
    render(<ExportScreen />);
    await screen.findByText("書式エラー 1 件・注意 0 件");
    fireEvent.click(screen.getByTestId("export-start"));
    await screen.findByText("確認事項を直すと書き出せます");
    expect(screen.queryByRole("button", { name: "このまま書き出す" })).toBeNull();
  });

  it("判定したMinecraftバージョンをSelectの初期値にする", async () => {
    render(<ExportScreen />);
    const versionSelect = await screen.findByRole("combobox", { name: "Minecraft バージョン" });
    expect(versionSelect.textContent).toBe("1.21.1");
    expect(screen.getByText("Mod の対応範囲 >=1.21 から判定")).toBeTruthy();
    await waitFor(() => {
      expect(vi.mocked(validateSelectedProject).mock.calls.at(-1)).toEqual([
        "project-1",
        "resourcePack",
        [1],
        "1.21.1",
        undefined,
        undefined,
      ]);
    });
  });

  it("古い初期読み込みの結果を別プロジェクトのstoreへ反映しない", async () => {
    const firstSummary = deferred<ProjectDetailsDto>();
    const firstUnits = deferred<ReturnType<typeof useProjectStore.getState>["modUnits"]>();
    const firstOverview = deferred<Awaited<ReturnType<typeof getExportOverview>>>();
    vi.mocked(getProjectSummary).mockReturnValueOnce(firstSummary.promise).mockResolvedValue(details("project-2"));
    vi.mocked(listModUnits).mockReturnValueOnce(firstUnits.promise).mockResolvedValue([modUnit(2)]);
    vi.mocked(getExportOverview).mockReturnValueOnce(firstOverview.promise);

    render(<ExportScreen />);
    await waitFor(() => expect(getExportOverview).toHaveBeenCalledWith("project-1"));
    act(() => {
      useProjectStore.getState().setProjectDetails(details("project-2"));
      useProjectStore.getState().setModUnits([modUnit(2)]);
    });
    await waitFor(() => expect(getExportOverview).toHaveBeenCalledWith("project-2"));
    await screen.findByTestId("export-start");

    await act(async () => {
      firstSummary.resolve(details());
      firstUnits.resolve([modUnit(1)]);
      firstOverview.resolve({ instancePath: null, units: [], activeReplacements: [] });
      await Promise.all([firstSummary.promise, firstUnits.promise, firstOverview.promise]);
    });

    expect(useProjectStore.getState().details?.project.id).toBe("project-2");
    expect(useProjectStore.getState().modUnits.map((unit) => unit.id)).toEqual([2]);
  });

  it("検証待ちにプロジェクトが変わったら書き出しを開始しない", async () => {
    const pendingValidation = deferred<Awaited<ReturnType<typeof validateSelectedProject>>>();
    render(<ExportScreen />);
    const start = await screen.findByTestId("export-start");
    vi.mocked(validateSelectedProject).mockReturnValueOnce(pendingValidation.promise);

    fireEvent.click(start);
    await waitFor(() => expect(validateSelectedProject).toHaveBeenLastCalledWith(
      "project-1",
      "resourcePack",
      [1],
      "1.21.1",
      undefined,
      undefined,
    ));
    act(() => useProjectStore.getState().setProjectDetails(details("project-2")));

    await act(async () => {
      pendingValidation.resolve({
        issues: [],
        minecraftVersionDetection: details().minecraftVersionDetection ?? null,
        counts: { total: 2, translated: 1, missing: 1, needsReview: 1 },
        isSigned: false,
      });
      await pendingValidation.promise;
    });

    expect(saveFileDialog).not.toHaveBeenCalled();
    expect(exportSelectedMergedResourcePack).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("保存先選択中に画面を離れたら書き出しコマンドを呼ばない", async () => {
    const pendingDestination = deferred<string | null>();
    vi.mocked(saveFileDialog).mockReturnValueOnce(pendingDestination.promise);
    const view = render(<ExportScreen />);

    fireEvent.click(await screen.findByTestId("export-start"));
    await waitFor(() => expect(saveFileDialog).toHaveBeenCalledOnce());
    view.unmount();

    await act(async () => {
      pendingDestination.resolve("C:\\exports\\pack.zip");
      await pendingDestination.promise;
    });

    expect(exportSelectedMergedResourcePack).not.toHaveBeenCalled();
  });

  it("復元確認待ちにプロジェクトが変わったら確認を表示しない", async () => {
    const pendingRestore = deferred<Awaited<ReturnType<typeof restoreModifiedJar>>>();
    vi.mocked(getExportOverview).mockResolvedValue({
      instancePath: null,
      units: [],
      activeReplacements: [{
        id: 9,
        modUnitId: 1,
        targetPath: "C:\\mods\\fixture.jar",
        backupPath: "C:\\project\\backups\\fixture.jar",
        previousSha256: "source-hash",
        writtenSha256: "written-hash",
        createdAt: "2026-01-01T00:00:00Z",
        restoredAt: null,
      }],
    });
    vi.mocked(restoreModifiedJar).mockReturnValueOnce(pendingRestore.promise);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<ExportScreen />);

    fireEvent.click(await screen.findByRole("button", { name: "元に戻す" }));
    await waitFor(() => expect(restoreModifiedJar).toHaveBeenCalledWith("project-1", 9, false));
    act(() => useProjectStore.getState().setProjectDetails(details("project-2")));

    await act(async () => {
      pendingRestore.resolve({ status: "confirmationRequired", targetPath: "C:\\mods\\fixture.jar" });
      await pendingRestore.promise;
    });

    expect(confirm).not.toHaveBeenCalled();
    expect(restoreModifiedJar).toHaveBeenCalledOnce();
    confirm.mockRestore();
  });
});
