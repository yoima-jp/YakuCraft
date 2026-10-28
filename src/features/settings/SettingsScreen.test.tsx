import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import userEvent from "@testing-library/user-event";

import {
  cancelChatgptLogin,
  detectMinecraftJars,
  getAiSettings,
  getAppInfo,
  getMinecraftJarSetting,
  getSettings,
  listAiModels,
  logoutChatgpt,
  saveAiProvider,
  saveSettings,
  startChatgptLogin,
} from "../../lib/tauri";
import { SettingsScreen } from "./SettingsScreen";
import { useAppStore } from "../../stores/appStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";

vi.mock("../../lib/tauri", () => ({
  AppCommandError: class extends Error {},
  clearCache: vi.fn(),
  cancelChatgptLogin: vi.fn(),
  detectMinecraftJars: vi.fn(),
  deleteAiProvider: vi.fn(),
  getAiSettings: vi.fn(),
  getAppInfo: vi.fn(),
  getMinecraftJarSetting: vi.fn(),
  getSettings: vi.fn(),
  listAiModels: vi.fn().mockResolvedValue([]),
  logoutChatgpt: vi.fn(),
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
      loaded: true,
      error: null,
    });
    vi.mocked(getSettings).mockResolvedValue({ schemaVersion: 1, ...SETTING_DEFAULTS });
    vi.mocked(saveSettings).mockResolvedValue(undefined);
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
