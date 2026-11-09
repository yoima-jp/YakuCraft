import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import userEvent from "@testing-library/user-event";

import {
  acceptPlamoLicense,
  cancelChatgptLogin,
  cancelPlamoDownload,
  deletePlamoModel,
  detectMinecraftJars,
  downloadPlamoFiles,
  getAiSettings,
  getAppInfo,
  getMinecraftJarSetting,
  getPlamoStatus,
  getSettings,
  listAiModels,
  logoutChatgpt,
  openPlamoLicense,
  saveAiProvider,
  saveSettings,
  startChatgptLogin,
} from "../../lib/tauri";
import { SettingsScreen } from "./SettingsScreen";
import { useAppStore } from "../../stores/appStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import type { AiSettingsDto, PlamoDownloadEvent, PlamoStatusDto } from "../../types/ai";

vi.mock("../../lib/tauri", () => ({
  AppCommandError: class extends Error {},
  clearCache: vi.fn(),
  cancelChatgptLogin: vi.fn(),
  cancelPlamoDownload: vi.fn(),
  acceptPlamoLicense: vi.fn(),
  deletePlamoModel: vi.fn(),
  detectMinecraftJars: vi.fn(),
  deleteAiProvider: vi.fn(),
  downloadPlamoFiles: vi.fn(),
  getAiSettings: vi.fn(),
  getAppInfo: vi.fn(),
  getMinecraftJarSetting: vi.fn(),
  getPlamoStatus: vi.fn(),
  getSettings: vi.fn(),
  getSystemLocale: vi.fn().mockResolvedValue("ja-JP"),
  listAiModels: vi.fn().mockResolvedValue([]),
  logoutChatgpt: vi.fn(),
  openPlamoLicense: vi.fn(),
  saveAiProvider: vi.fn(),
  saveSettings: vi.fn(),
  startChatgptLogin: vi.fn(),
  testAiProvider: vi.fn(),
}));

function settings(provider: "openai" | "chatgpt", configured: boolean, accountLabel?: string) {
  return {
    provider,
    configured,
    ...(accountLabel === undefined ? {} : { accountLabel }),
    model: provider === "chatgpt" ? "gpt-6-luna" : "example-model",
    baseUrl: "https://api.openai.com/v1",
    batchSize: 50,
    concurrency: 2,
    customPrompt: "",
  };
}

const PLAMO_MODELS = [
  { id: "plamo-3-translate-0.8b" as const, parameterLabel: "0.8B", sizeBytes: 612_000_000, downloaded: false },
  { id: "plamo-3-translate-2b" as const, parameterLabel: "2B", sizeBytes: 1_610_000_000, downloaded: false },
  { id: "plamo-3-translate-8b" as const, parameterLabel: "8B", sizeBytes: 5_370_000_000, downloaded: false },
];

function plamoSettings(overrides: Partial<AiSettingsDto> = {}): AiSettingsDto {
  return {
    provider: "plamo",
    configured: false,
    model: "plamo-3-translate-2b",
    baseUrl: "",
    batchSize: 50,
    concurrency: 2,
    customPrompt: "",
    device: "vulkan",
    ...overrides,
  };
}

function plamoStatus(overrides: Partial<PlamoStatusDto> = {}): PlamoStatusDto {
  return {
    models: PLAMO_MODELS,
    runtimeReady: { cpu: false, vulkan: false, cuda: null },
    runtimeSizeBytes: { cpu: 120_000_000, vulkan: 240_000_000, cuda: null },
    devices: { cudaOffered: false, gpuName: null, default: "vulkan" },
    licenseAccepted: false,
    supportedTargetLocales: ["ja_jp", "en_us", "de_de"],
    downloading: false,
    ...overrides,
  };
}

async function selectChatgpt() {
  fireEvent.click(screen.getByRole("combobox", { name: "プロバイダー" }));
  fireEvent.click(screen.getByRole("option", { name: "ChatGPT（サブスクリプション）" }));
  await waitFor(() => screen.getByRole("button", { name: /ChatGPTでログイン|ログアウト/ }));
}

async function selectOllama() {
  fireEvent.click(screen.getByRole("combobox", { name: "プロバイダー" }));
  fireEvent.click(screen.getByRole("option", { name: "Ollama" }));
  await waitFor(() => expect(screen.getByRole("textbox", { name: "モデル" })).toBeTruthy());
}

describe("SettingsScreen API Key field", () => {
  let chatgptConfigured: boolean;

  beforeEach(() => {
    vi.clearAllMocks();
    window.localStorage.clear();
    chatgptConfigured = false;
    useAppStore.setState({ settingsCategory: "ai" });
    useSettingsStore.setState({
      settings: { schemaVersion: 1, ...SETTING_DEFAULTS },
      savedValues: {},
      loaded: true,
      error: null,
    });
    vi.mocked(getSettings).mockResolvedValue({ schemaVersion: 1, ...SETTING_DEFAULTS });
    vi.mocked(saveSettings).mockResolvedValue(undefined);
    // 既定が PLaMo なので、AI 設定の読み込み前は PLaMo のパネルとして状態を取りに行く。
    vi.mocked(getPlamoStatus).mockResolvedValue(plamoStatus());
    vi.mocked(getAiSettings).mockImplementation(async (provider) =>
      provider === "chatgpt"
        ? settings("chatgpt", chatgptConfigured, chatgptConfigured ? "user@example.com" : undefined)
        : settings("openai", true),
    );
    vi.mocked(getAppInfo).mockResolvedValue({
      appName: "YakuCraft",
      version: "0.1.0",
      dataDirectory: "C:/app-data",
    });
    vi.mocked(startChatgptLogin).mockImplementation(async () => {
      chatgptConfigured = true;
      return { configured: true, provider: "chatgpt", accountLabel: "user@example.com" };
    });
    vi.mocked(cancelChatgptLogin).mockResolvedValue(true);
    vi.mocked(logoutChatgpt).mockImplementation(async () => {
      chatgptConfigured = false;
    });
    vi.mocked(saveAiProvider).mockImplementation(async (config) => ({
      ...settings(config.provider as "openai" | "chatgpt", chatgptConfigured),
      ...config,
      configured: chatgptConfigured,
      ...(chatgptConfigured ? { accountLabel: "user@example.com" } : {}),
    }));
  });

  afterEach(() => cleanup());

  it("AIの入力値をStrictModeでも編集して保存できる", async () => {
    vi.mocked(getAiSettings).mockResolvedValue({ ...settings("openai", true), provider: "openai-compatible" });
    const user = userEvent.setup();
    render(<StrictMode><SettingsScreen /></StrictMode>);
    const model = await screen.findByRole("textbox", { name: "モデル" });
    await user.clear(model);
    await user.type(model, "new-model");
    const url = screen.getByRole("textbox", { name: /接続先URL/ });
    await user.clear(url);
    await user.type(url, "https://example.com/v1");
    await user.click(screen.getByRole("button", { name: "詳細設定" }));
    fireEvent.change(screen.getByRole("spinbutton", { name: "1回の送信件数" }), { target: { value: "12" } });
    await user.type(screen.getByRole("textbox", { name: "追加指示" }), "自然な日本語で翻訳");
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(saveAiProvider).toHaveBeenCalledWith(expect.objectContaining({
      model: "new-model", baseUrl: "https://example.com/v1", batchSize: 12, customPrompt: "自然な日本語で翻訳",
    }), undefined));
  }, 10_000);

  it.each([{ isComposing: true }, { keyCode: 229 }])("IMEのEscapeでは設定を閉じない (%j)", async (composition) => {
    useAppStore.setState({ route: "settings", settingsReturnRoute: "editor" });
    render(<SettingsScreen />);
    await screen.findByRole("textbox", { name: "モデル" });
    fireEvent.click(screen.getByRole("button", { name: "詳細設定" }));
    fireEvent.keyDown(screen.getByRole("textbox", { name: "追加指示" }), { key: "Escape", ...composition });
    expect(useAppStore.getState().route).toBe("settings");
    fireEvent.keyDown(screen.getByRole("textbox", { name: "追加指示" }), { key: "Escape" });
    await waitFor(() => expect(useAppStore.getState().route).toBe("editor"));
  });

  it("ライト・ダーク・システムを選べて、ダークを即時反映して次回読込にも残す", async () => {
    useAppStore.setState({ settingsCategory: "appearance" });
    useSettingsStore.setState((state) => ({
      settings: { ...state.settings, "appearance.theme": "light" },
    }));
    render(<SettingsScreen />);

    const light = screen.getByRole("radio", { name: "ライト" }) as HTMLInputElement;
    const dark = screen.getByRole("radio", { name: "ダーク" }) as HTMLInputElement;
    const system = screen.getByRole("radio", { name: "システムに合わせる" }) as HTMLInputElement;
    expect(light.checked).toBe(true);
    expect(dark.checked).toBe(false);
    expect(system.checked).toBe(false);

    fireEvent.click(dark);

    await waitFor(() => {
      expect(dark.checked).toBe(true);
      expect(useSettingsStore.getState().settings["appearance.theme"]).toBe("dark");
    });
    expect(saveSettings).toHaveBeenCalledWith({ "appearance.theme": "dark" });
    expect(window.localStorage.getItem("yakucraft.appearance.theme")).toBe("dark");

    vi.mocked(getSettings).mockResolvedValue({
      schemaVersion: 1,
      ...SETTING_DEFAULTS,
      "appearance.theme": "dark",
    });
    await useSettingsStore.getState().load();
    expect(useSettingsStore.getState().settings["appearance.theme"]).toBe("dark");
    expect(window.localStorage.getItem("yakucraft.appearance.theme")).toBe("dark");
  });

  it("Minecraft JARの説明にプレビューへの効果だけを表示する", async () => {
    vi.mocked(detectMinecraftJars).mockResolvedValue([]);
    vi.mocked(getMinecraftJarSetting).mockResolvedValue(null);
    useAppStore.setState({ settingsCategory: "advanced" });
    render(<SettingsScreen />);

    expect(await screen.findByText(
      "Minecraft 本体の JAR を指定すると、バニラのブロックやテクスチャもプレビューに表示されます。",
    )).toBeTruthy();
    expect(screen.queryByText(/再配布/)).toBeNull();
  });

  it("保存済みKeyの値をWebViewに返さず設定状態だけを示す", async () => {
    render(<SettingsScreen />);
    const keyField = await waitFor(() => screen.getByLabelText(/APIキー/)) as HTMLInputElement;

    expect(keyField.type).toBe("password");
    expect(keyField.value).toBe("");
    expect(screen.getByText("設定済み")).toBeTruthy();
  });

  it("保存済みキーが入力欄で空でも、接続先変更時にモデル候補を取り直す", async () => {
    let resolveUpdatedModels!: (models: string[]) => void;
    vi.mocked(getAiSettings).mockResolvedValue({
      ...settings("openai", true),
      provider: "openai-compatible",
    });
    vi.mocked(listAiModels)
      .mockResolvedValueOnce(["old-model"])
      .mockImplementationOnce(() => new Promise((resolve) => { resolveUpdatedModels = resolve; }));
    const user = userEvent.setup();
    render(<SettingsScreen />);

    await waitFor(() => expect(screen.getByRole("button", { name: "候補を表示" })).toBeTruthy());
    await user.click(screen.getByRole("button", { name: "候補を表示" }));
    expect(screen.getByRole("option", { name: "old-model" })).toBeTruthy();
    const url = screen.getByRole("textbox", { name: /接続先URL/ });
    await user.clear(url);
    await user.type(url, "http://127.0.0.1:43210/v1");

    await waitFor(() => expect(listAiModels).toHaveBeenCalledTimes(2));
    expect(listAiModels).toHaveBeenLastCalledWith(
      "openai-compatible",
      "http://127.0.0.1:43210/v1",
      undefined,
    );
    expect(screen.queryByRole("button", { name: "候補を表示" })).toBeNull();

    resolveUpdatedModels(["new-model"]);
    await waitFor(() => expect(screen.getByRole("button", { name: "候補を表示" })).toBeTruthy());
    await user.click(screen.getByRole("button", { name: "候補を表示" }));
    expect(screen.getByRole("option", { name: "new-model" })).toBeTruthy();
  }, 10_000);

  it("初めてOllamaを選んだとき、未設定でもモデル一覧を取得する", async () => {
    vi.mocked(getAiSettings).mockImplementation(async (provider) => provider === "ollama"
      ? { ...settings("openai", false), provider: "ollama", model: "", baseUrl: "http://localhost:11434/v1" }
      : settings("chatgpt", false));
    vi.mocked(listAiModels).mockResolvedValue(["llama3.2"]);
    render(<SettingsScreen />);

    await screen.findByRole("button", { name: "ChatGPTでログイン" });
    await selectOllama();

    await waitFor(() => expect(listAiModels).toHaveBeenCalledWith(
      "ollama",
      "http://localhost:11434/v1",
      undefined,
    ));
    await waitFor(() => expect(screen.getByRole("button", { name: "候補を表示" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "候補を表示" }));
    expect(screen.getByRole("option", { name: "llama3.2" })).toBeTruthy();
  });

  it("モデル一覧の取得に失敗したら短い案内を表示して手入力を残す", async () => {
    vi.mocked(getAiSettings).mockImplementation(async (provider) => provider === "ollama"
      ? { ...settings("openai", false), provider: "ollama", model: "", baseUrl: "http://localhost:11434/v1" }
      : settings("chatgpt", false));
    vi.mocked(listAiModels).mockRejectedValue(new Error("connection refused"));
    render(<SettingsScreen />);

    await screen.findByRole("button", { name: "ChatGPTでログイン" });
    await selectOllama();

    expect((await screen.findByRole("alert")).textContent).toBe("モデル一覧を取得できません。モデル名を入力してください。");
    const model = screen.getByRole("textbox", { name: "モデル" }) as HTMLInputElement;
    fireEvent.change(model, { target: { value: "manual-model" } });
    expect(model.value).toBe("manual-model");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("ChatGPTではAPIキー欄の代わりにログインとモデル入力を表示する", async () => {
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectChatgpt();

    expect(screen.getByRole("button", { name: "ChatGPTでログイン" })).toBeTruthy();
    expect(screen.queryByLabelText(/APIキー/)).toBeNull();
    expect((screen.getByLabelText("モデル") as HTMLInputElement).value).toBe("gpt-6-luna");
  });

  it("ログイン中は待機表示とキャンセルを出し、Tauriのログイン処理をキャンセルする", async () => {
    let rejectLogin!: (error: Error) => void;
    vi.mocked(startChatgptLogin).mockReturnValue(new Promise((_resolve, reject) => {
      rejectLogin = reject;
    }));
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectChatgpt();

    fireEvent.click(screen.getByRole("button", { name: "ChatGPTでログイン" }));
    expect(screen.getByRole("status").textContent).toContain("ブラウザでログインしてください");
    fireEvent.click(screen.getByRole("button", { name: "キャンセル" }));

    await waitFor(() => expect(cancelChatgptLogin).toHaveBeenCalledOnce());
    rejectLogin(new Error("cancelled"));
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  });

  it("ログイン後にアカウントを表示して設定を保存する", async () => {
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectChatgpt();

    fireEvent.click(screen.getByRole("button", { name: "ChatGPTでログイン" }));

    await waitFor(() => expect(screen.getByText("user@example.com")).toBeTruthy());
    expect(startChatgptLogin).toHaveBeenCalledOnce();
    expect(saveAiProvider).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "ログアウト" })).toBeTruthy();
  });

  it("ログアウト後は再ログインの操作を表示する", async () => {
    chatgptConfigured = true;
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectChatgpt();

    fireEvent.click(screen.getByRole("button", { name: "ログアウト" }));

    await waitFor(() => expect(screen.getByRole("button", { name: "ChatGPTでログイン" })).toBeTruthy());
    expect(logoutChatgpt).toHaveBeenCalledOnce();
  });

  it("エディター設定からInspectorプレビューの表示を保存する", async () => {
    useAppStore.setState({ settingsCategory: "editor" });
    render(<SettingsScreen />);
    const checkbox = screen.getByRole("checkbox", { name: "詳細のプレビューを非表示" }) as HTMLInputElement;
    expect(checkbox.checked).toBe(false);

    fireEvent.click(checkbox);

    await waitFor(() => {
      expect(checkbox.checked).toBe(true);
      expect(useSettingsStore.getState().settings["editor.hideInspectorPreview"]).toBe(true);
    });
  });

  it("Escapeで設定を閉じて開いていた画面へ戻る", async () => {
    useAppStore.setState({ route: "settings", settingsReturnRoute: "export" });
    render(<SettingsScreen />);

    fireEvent.keyDown(window, { key: "Escape" });

    await waitFor(() => expect(useAppStore.getState().route).toBe("export"));
  });
});

describe("SettingsScreen PLaMo panel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.localStorage.clear();
    useAppStore.setState({ settingsCategory: "ai" });
    useSettingsStore.setState({
      settings: { schemaVersion: 1, ...SETTING_DEFAULTS, "ai.provider": "plamo" },
      savedValues: { "ai.provider": "plamo" },
      loaded: true,
      error: null,
    });
    vi.mocked(getSettings).mockResolvedValue({ schemaVersion: 1, ...SETTING_DEFAULTS, "ai.provider": "plamo" });
    vi.mocked(saveSettings).mockResolvedValue(undefined);
    vi.mocked(getAiSettings).mockResolvedValue(plamoSettings());
    vi.mocked(saveAiProvider).mockImplementation(async (config) => plamoSettings(config));
    vi.mocked(getPlamoStatus).mockResolvedValue(plamoStatus());
    vi.mocked(acceptPlamoLicense).mockResolvedValue(undefined);
    vi.mocked(downloadPlamoFiles).mockResolvedValue(undefined);
    vi.mocked(cancelPlamoDownload).mockResolvedValue(true);
    vi.mocked(deletePlamoModel).mockResolvedValue(undefined);
    vi.mocked(openPlamoLicense).mockResolvedValue(undefined);
    vi.mocked(getAppInfo).mockResolvedValue({
      appName: "YakuCraft",
      version: "0.1.0",
      dataDirectory: "C:/app-data",
    });
  });

  afterEach(() => cleanup());

  async function selectPlamo() {
    fireEvent.click(screen.getByRole("combobox", { name: "プロバイダー" }));
    fireEvent.click(screen.getByRole("option", { name: "PLaMo（ローカル）" }));
    await waitFor(() => expect(screen.getByRole("combobox", { name: "モデル" })).toBeTruthy());
  }

  it("モデルとデバイスとダウンロードを表示し、APIキー・追加指示・送信件数・削除を出さない", async () => {
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    expect(screen.getByRole("combobox", { name: "モデル" })).toBeTruthy();
    expect(screen.getByRole("combobox", { name: "デバイス" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /ダウンロード/ })).toBeTruthy();
    expect(screen.queryByLabelText(/APIキー/)).toBeNull();
    expect(screen.queryByRole("textbox", { name: "モデル" })).toBeNull();
    expect(screen.queryByRole("textbox", { name: "接続先URL" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "詳細設定" }));
    expect(screen.queryByRole("spinbutton", { name: "1回の送信件数" })).toBeNull();
    expect(screen.queryByRole("textbox", { name: "追加指示" })).toBeNull();
    expect(screen.getByRole("combobox", { name: "並列数" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "削除" })).toBeNull();
  });

  it("ライセンス未同意では確認ダイアログを挟み、同意してからダウンロードを開始する", async () => {
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    fireEvent.click(screen.getByRole("button", { name: /ダウンロード/ }));

    const dialog = await screen.findByRole("dialog", { name: "PLaMo のライセンス" });
    expect(dialog.textContent).toContain("Preferred Networks");
    expect(downloadPlamoFiles).not.toHaveBeenCalled();
    expect(acceptPlamoLicense).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "同意してダウンロード" }));

    await waitFor(() => expect(acceptPlamoLicense).toHaveBeenCalledOnce());
    await waitFor(() => expect(downloadPlamoFiles).toHaveBeenCalledWith(
      "plamo-3-translate-2b",
      "vulkan",
      expect.any(Function),
    ));
  });

  it("ライセンス同意済みならダイアログを出さずにダウンロードする", async () => {
    vi.mocked(getPlamoStatus).mockResolvedValue(plamoStatus({ licenseAccepted: true }));
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    fireEvent.click(screen.getByRole("button", { name: /ダウンロード/ }));

    await waitFor(() => expect(downloadPlamoFiles).toHaveBeenCalledOnce());
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(acceptPlamoLicense).not.toHaveBeenCalled();
  });

  it("進捗イベントでバーとフェーズ表示を更新し、キャンセルを呼ぶ", async () => {
    vi.mocked(getPlamoStatus).mockResolvedValue(plamoStatus({ licenseAccepted: true }));
    let emit!: (event: PlamoDownloadEvent) => void;
    vi.mocked(downloadPlamoFiles).mockImplementation((_modelId, _device, onEvent) => {
      emit = onEvent;
      return new Promise(() => {});
    });
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    fireEvent.click(screen.getByRole("button", { name: /ダウンロード/ }));
    await waitFor(() => expect(downloadPlamoFiles).toHaveBeenCalledOnce());

    act(() => emit({ kind: "progress", phase: "model", receivedBytes: 1024 ** 3, totalBytes: 2 * 1024 ** 3 }));

    const bar = await screen.findByRole("progressbar", { name: "ダウンロードの進行状況" });
    expect(bar.getAttribute("value")).toBe("50");
    expect(screen.getByText(/モデル · 1 GB \/ 2 GB/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "キャンセル" }));
    await waitFor(() => expect(cancelPlamoDownload).toHaveBeenCalledOnce());
  });

  it("ダウンロード失敗時はバックエンドのメッセージを出し、再試行できる", async () => {
    vi.mocked(getPlamoStatus).mockResolvedValue(plamoStatus({ licenseAccepted: true }));
    let emit!: (event: PlamoDownloadEvent) => void;
    vi.mocked(downloadPlamoFiles).mockImplementation(async (_modelId, _device, onEvent) => {
      emit = onEvent;
    });
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    fireEvent.click(screen.getByRole("button", { name: /ダウンロード/ }));
    await waitFor(() => expect(downloadPlamoFiles).toHaveBeenCalledOnce());

    act(() => emit({
      kind: "failed",
      error: { code: "plamo_download_failed", userMessage: "接続を確認してください。", recoverable: true },
    }));

    expect((await screen.findByRole("alert")).textContent).toBe("接続を確認してください。");
    fireEvent.click(screen.getByRole("button", { name: /ダウンロード/ }));
    await waitFor(() => expect(downloadPlamoFiles).toHaveBeenCalledTimes(2));
  });

  it("ダウンロード済みモデルは削除でき、確認後にモデル削除を呼ぶ", async () => {
    vi.mocked(getPlamoStatus).mockResolvedValue(plamoStatus({
      licenseAccepted: true,
      models: PLAMO_MODELS.map((model) => ({ ...model, downloaded: true })),
      runtimeReady: { cpu: true, vulkan: true, cuda: null },
    }));
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    expect(screen.queryByRole("button", { name: /ダウンロード/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "削除" }));

    const dialog = await screen.findByRole("dialog", { name: "モデルを削除しますか？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "削除" }));

    await waitFor(() => expect(deletePlamoModel).toHaveBeenCalledWith("plamo-3-translate-2b"));
  });

  it("Built with PLaMo の表示からライセンスを開き、日本語UIではjaを渡す", async () => {
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    expect(screen.getByText("Built with PLaMo")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "ライセンス" }));

    await waitFor(() => expect(openPlamoLicense).toHaveBeenCalledWith("ja"));
  });

  it("保存するとdeviceを含めて保存し、PLaMoの設定と状態を取り直す", async () => {
    // 契約ではPLaMoの接続先と追加指示は空文字なので、画面の値が残っていても空で送ることを確かめる。
    vi.mocked(getAiSettings).mockResolvedValue(plamoSettings({
      baseUrl: "https://stale.example.com/v1",
      customPrompt: "古い追加指示",
    }));
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    fireEvent.click(screen.getByRole("combobox", { name: "デバイス" }));
    fireEvent.click(screen.getByRole("option", { name: "CPU" }));
    const statusCallsBeforeSave = vi.mocked(getPlamoStatus).mock.calls.length;
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(saveAiProvider).toHaveBeenCalledWith(
      expect.objectContaining({ provider: "plamo", model: "plamo-3-translate-2b", device: "cpu", baseUrl: "", customPrompt: "" }),
      undefined,
    ));
    await waitFor(() => expect(getAiSettings).toHaveBeenCalledWith("plamo"));
    await waitFor(() => expect(vi.mocked(getPlamoStatus).mock.calls.length).toBeGreaterThan(statusCallsBeforeSave));
  });

  it("翻訳先の選択肢でPLaMo非対応言語を選べなくする", async () => {
    useAppStore.setState({ settingsCategory: "language" });
    render(<SettingsScreen />);

    await waitFor(() => expect(getPlamoStatus).toHaveBeenCalled());
    fireEvent.focus(screen.getByRole("combobox", { name: "翻訳先の言語" }));

    const unsupported = await screen.findByRole("option", { name: /Afrikaans/ });
    expect(unsupported.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(unsupported);
    expect(saveSettings).not.toHaveBeenCalledWith(expect.objectContaining({ "language.targetLocale": "af_za" }));

    const supported = screen.getByRole("option", { name: /Deutschland/ });
    expect(supported.getAttribute("aria-disabled")).toBeNull();
  });

  it("CUDAが使える環境でだけGPU（CUDA）を選択肢に出し、既定はバックエンドに従う", async () => {
    // CUDA を提供しない環境では選択肢を出さない。選んでも準備できないため。
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();
    fireEvent.click(screen.getByRole("combobox", { name: "デバイス" }));
    expect(screen.queryByRole("option", { name: "GPU（CUDA）" })).toBeNull();
    expect(screen.getByRole("option", { name: "GPU（Vulkan）" })).toBeTruthy();
    fireEvent.click(screen.getByRole("option", { name: "GPU（Vulkan）" }));
  });

  it("CUDAが使える環境ではGPU（CUDA）を出し、未保存ならそれを既定にする", async () => {
    vi.mocked(getPlamoStatus).mockResolvedValue(plamoStatus({
      runtimeReady: { cpu: false, vulkan: false, cuda: false },
      runtimeSizeBytes: { cpu: 120_000_000, vulkan: 240_000_000, cuda: 592_000_000 },
      devices: { cudaOffered: true, gpuName: "NVIDIA GeForce RTX 3060", default: "cuda" },
    }));
    // device 未指定（未保存）で開くと、バックエンドの既定に従う。
    vi.mocked(getAiSettings).mockResolvedValue(plamoSettings({ device: undefined }));
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    const select = screen.getByRole("combobox", { name: "デバイス" });
    expect(select.textContent).toContain("GPU（CUDA）");
    fireEvent.click(select);
    expect(screen.getByRole("option", { name: "GPU（CUDA）" })).toBeTruthy();
    fireEvent.click(screen.getByRole("option", { name: "GPU（CUDA）" }));

    // ダウンロード量は CUDA の 2 アーカイブ合計（592 MB）+ モデル（1.61 GB）。
    expect(screen.getByRole("button", { name: /ダウンロード/ }).textContent).toContain("2.051");
  });

  it("旧値gpuはGPU（Vulkan）として表示する", async () => {
    // 旧バージョンの保存値。読み替えないと選択肢と一致せず、保存し直すまで空表示になる。
    vi.mocked(getAiSettings).mockResolvedValue(plamoSettings({ device: "gpu" as never }));
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    expect(screen.getByRole("combobox", { name: "デバイス" }).textContent).toContain("GPU（Vulkan）");
  });

  it("CUDAが使えない環境に保存されたcudaは、表示だけでなく保存値も代替デバイスへ直す", async () => {
    // 表示だけ既定へ寄せると、ダウンロードは Vulkan なのに保存値は cuda のままになり、
    // 取得しても「未準備」が続く。表示した代替デバイスを保存にも反映する。
    vi.mocked(getAiSettings).mockResolvedValue(plamoSettings({ device: "cuda" }));
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    // cudaOffered: false の既定は vulkan（plamoStatus の devices.default）。
    expect(screen.getByRole("combobox", { name: "デバイス" }).textContent).toContain("GPU（Vulkan）");

    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(saveAiProvider).toHaveBeenCalledWith(
      expect.objectContaining({ provider: "plamo", device: "vulkan" }),
      undefined,
    ));
  });

  it("VulkanのヒントはVulkanを選んだときだけ出す", async () => {
    render(<SettingsScreen />);
    await waitFor(() => screen.getByRole("combobox", { name: "プロバイダー" }));
    await selectPlamo();

    // 既定は Vulkan なのでヒントが出る。
    expect(screen.getByText("GPU には Vulkan 対応のドライバーが必要です。")).toBeTruthy();
    fireEvent.click(screen.getByRole("combobox", { name: "デバイス" }));
    fireEvent.click(screen.getByRole("option", { name: "CPU" }));
    expect(screen.queryByText("GPU には Vulkan 対応のドライバーが必要です。")).toBeNull();
  });

  it("PLaMoが既定値のままで未保存なら翻訳先を制限しない", async () => {
    // 既定が PLaMo でも、明示的に保存していない利用者の翻訳先を絞ると、Minecraft の
    // 言語の半分近くが最初から選べなくなる。保存値が無い状態を作って確かめる。
    useAppStore.setState({ settingsCategory: "language" });
    useSettingsStore.setState({ savedValues: {} });
    vi.mocked(getSettings).mockResolvedValue({ schemaVersion: 1, ...SETTING_DEFAULTS });
    vi.mocked(getAiSettings).mockResolvedValue(plamoSettings());
    render(<SettingsScreen />);

    fireEvent.focus(await screen.findByRole("combobox", { name: "翻訳先の言語" }));

    const portuguese = (await screen.findAllByRole("option"))
      .find((option) => option.querySelector("code")?.textContent === "pt_br");
    expect(portuguese).toBeTruthy();
    expect(portuguese?.getAttribute("aria-disabled")).toBeNull();
  });
});
