import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { openOutputFolder } from "../../lib/tauri";
import { useAppStore } from "../../stores/appStore";
import type { ExportCompletion } from "../../stores/appStore";
import { ExportCompletionScreen } from "./ExportCompletionScreen";

vi.mock("../../lib/tauri", () => ({
  AppCommandError: class extends Error {},
  openOutputFolder: vi.fn(),
}));

const completion: ExportCompletion = {
  kind: "resourcePack",
  outputPath: "C:\\exports\\fixture-JP_EX.zip",
  translatedCount: 24,
  missingCount: 3,
  namespace: "fixture",
  autoOpenOutputFolder: false,
};

beforeEach(() => {
  vi.clearAllMocks();
  useAppStore.setState({
    route: "exportComplete",
    settingsReturnRoute: "home",
    exportCompletion: { ...completion },
  });
});

afterEach(() => cleanup());

describe("ExportCompletionScreen", () => {
  it.each([
    ["resourcePack", "リソースパック", "Minecraft のリソースパック設定で有効にしてください"],
    ["modifiedJar", "Mod JAR", "元のJARは変更していません"],
    ["languageFile", "言語ファイル", "配置先: assets/fixture/lang/"],
  ] as const)("%sの案内と出力情報を表示する", (kind, format, instruction) => {
    useAppStore.getState().setExportCompletion({ ...completion, kind });
    render(<ExportCompletionScreen />);

    expect(screen.getByRole("heading", { name: "書き出しました" })).toBeTruthy();
    expect(screen.getByText(format)).toBeTruthy();
    expect(screen.getByText("fixture-JP_EX.zip")).toBeTruthy();
    expect(screen.getByText("C:\\exports")).toBeTruthy();
    expect(screen.getByText(instruction)).toBeTruthy();
    expect(screen.getByText("24 件 翻訳済み")).toBeTruthy();
    expect(screen.getByText("未翻訳 合計 3 件")).toBeTruthy();
  });

  it("未翻訳がなければ件数を表示しない", () => {
    useAppStore.getState().setExportCompletion({ ...completion, missingCount: 0 });
    render(<ExportCompletionScreen />);

    expect(screen.getByText("24 件 翻訳済み")).toBeTruthy();
    expect(screen.queryByText(/未翻訳/)).toBeNull();
  });

  it("フォルダを開き、翻訳または形式選択へ戻れる", async () => {
    vi.mocked(openOutputFolder).mockResolvedValue(undefined);
    render(<ExportCompletionScreen />);

    fireEvent.click(screen.getByRole("button", { name: "フォルダを開く" }));
    await vi.waitFor(() => expect(openOutputFolder).toHaveBeenCalledWith(completion.outputPath));

    fireEvent.click(screen.getByRole("button", { name: "別の形式で書き出す" }));
    await vi.waitFor(() => expect(useAppStore.getState().route).toBe("export"));
    expect(useAppStore.getState().exportCompletion).toBeNull();
  });

  it("翻訳に戻る", async () => {
    render(<ExportCompletionScreen />);
    fireEvent.click(screen.getByRole("button", { name: "翻訳に戻る" }));
    await vi.waitFor(() => expect(useAppStore.getState().route).toBe("editor"));
  });

  it("設定が有効なら完了画面を表示しながら保存先を開く", async () => {
    vi.mocked(openOutputFolder).mockResolvedValue(undefined);
    useAppStore.getState().setExportCompletion({ ...completion, autoOpenOutputFolder: true });

    render(<ExportCompletionScreen />);

    expect(screen.getByRole("heading", { name: "書き出しました" })).toBeTruthy();
    await vi.waitFor(() => expect(openOutputFolder).toHaveBeenCalledWith(completion.outputPath));
  });
});
