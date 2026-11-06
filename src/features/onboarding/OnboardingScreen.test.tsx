import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { saveSettings } from "../../lib/tauri";
import { setUiLanguage } from "../../i18n";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import { OnboardingScreen } from "./OnboardingScreen";

vi.mock("../../lib/tauri", () => ({
  getSettings: vi.fn(),
  getSystemLocale: vi.fn(),
  saveSettings: vi.fn(),
}));

function setOnboardingState(language: "en" | "ko", systemLocale: string) {
  setUiLanguage(language);
  useSettingsStore.setState({
    settings: {
      schemaVersion: 1,
      ...SETTING_DEFAULTS,
      "language.uiLanguage": language,
      "language.targetLocale": "de_de",
      "onboarding.completed": false,
    },
    loaded: true,
    error: null,
    systemLocale,
  });
}

describe("OnboardingScreen", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(saveSettings).mockResolvedValue(undefined);
    setOnboardingState("en", "de-DE");
  });

  afterEach(() => {
    cleanup();
    setUiLanguage("ja");
  });

  it("英語では翻訳先の手順を表示し、3つの設定を1回で保存する", async () => {
    render(<OnboardingScreen />);

    expect(screen.getByRole("heading", { name: "Welcome to YakuCraft" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByRole("combobox", { name: "Translation language" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Finish" }));

    await waitFor(() => expect(saveSettings).toHaveBeenCalledOnce());
    expect(saveSettings).toHaveBeenCalledWith({
      "language.uiLanguage": "en",
      "language.targetLocale": "de_de",
      "onboarding.completed": true,
    });
  });

  it("韓国語では翻訳先の手順を飛ばして自動設定を保存する", async () => {
    setOnboardingState("ko", "en-US");
    render(<OnboardingScreen />);

    expect(screen.getByRole("heading", { name: "YakuCraft에 오신 것을 환영합니다" })).toBeTruthy();
    expect(screen.queryByRole("combobox")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "완료" }));

    await waitFor(() => expect(saveSettings).toHaveBeenCalledOnce());
    expect(saveSettings).toHaveBeenCalledWith({
      "language.uiLanguage": "ko",
      "language.targetLocale": "ko_kr",
      "onboarding.completed": true,
    });
  });

  it("保存に失敗した場合は画面に留まり、再試行できる", async () => {
    vi.mocked(saveSettings).mockRejectedValueOnce(new Error("write failed"));
    render(<OnboardingScreen />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Finish" }));

    expect((await screen.findByRole("alert")).textContent).toBe("Could not save settings. Try again.");
    expect(useSettingsStore.getState().settings["onboarding.completed"]).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Finish" }));
    await waitFor(() => expect(saveSettings).toHaveBeenCalledTimes(2));
  });
});
