import { beforeEach, describe, expect, it, vi } from "vitest";

import { getSettings, saveSettings } from "../lib/tauri";
import { readModTabPreferences, writeModTabPreferences } from "../features/translation/modTabState";
import { SETTING_DEFAULTS, useSettingsStore } from "./settingsStore";

vi.mock("../lib/tauri", () => ({
  getSettings: vi.fn(),
  getSystemLocale: vi.fn().mockResolvedValue("ja-JP"),
  saveSettings: vi.fn(),
}));

const projectId = "project-layout";
const validIds = [11, 12, 13];

describe("editor mod tab settings", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    useSettingsStore.setState({
      settings: { schemaVersion: 1, ...SETTING_DEFAULTS },
      loaded: false,
      error: null,
    });
    vi.mocked(saveSettings).mockResolvedValue(undefined);
  });

  it("保存した左配置を再読み込み後に復元する", async () => {
    vi.mocked(getSettings).mockResolvedValue({ schemaVersion: 1, "editor.modTabPosition": "left" });

    await useSettingsStore.getState().load();

    expect(useSettingsStore.getState().settings["editor.modTabPosition"]).toBe("left");
    expect(SETTING_DEFAULTS["editor.modTabPosition"]).toBe("top");
  });

  it("上と左を切り替えてもピン留めと開いているタブを保つ", async () => {
    const tabPreferences = { openIds: [11, 13], pinnedIds: [13], activeId: 13 };
    writeModTabPreferences(projectId, tabPreferences);

    await useSettingsStore.getState().save({ "editor.modTabPosition": "left" });
    await useSettingsStore.getState().save({ "editor.modTabPosition": "top" });

    expect(saveSettings).toHaveBeenNthCalledWith(1, { "editor.modTabPosition": "left" });
    expect(saveSettings).toHaveBeenNthCalledWith(2, { "editor.modTabPosition": "top" });
    expect(readModTabPreferences(projectId, validIds)).toEqual(tabPreferences);
  });

  it("無効な翻訳先をOS言語に補正して一度保存する", async () => {
    vi.mocked(getSettings).mockResolvedValue({
      schemaVersion: 1,
      "language.targetLocale": "not_a_locale",
    });

    await useSettingsStore.getState().load();

    expect(useSettingsStore.getState().settings["language.targetLocale"]).toBe("ja_jp");
    expect(saveSettings).toHaveBeenCalledTimes(1);
    expect(saveSettings).toHaveBeenCalledWith({ "language.targetLocale": "ja_jp" });
  });

  it("大文字やハイフンを含む有効な翻訳先は正規化して使う", async () => {
    vi.mocked(getSettings).mockResolvedValue({
      schemaVersion: 1,
      "language.targetLocale": "DE-DE",
    });

    await useSettingsStore.getState().load();

    expect(useSettingsStore.getState().settings["language.targetLocale"]).toBe("de_de");
    expect(saveSettings).not.toHaveBeenCalled();
  });

  it("オンボーディング中は無効な翻訳先を保存しない", async () => {
    vi.mocked(getSettings).mockResolvedValue({
      schemaVersion: 1,
      "language.targetLocale": "not_a_locale",
      "onboarding.completed": false,
    });

    await useSettingsStore.getState().load();

    expect(useSettingsStore.getState().settings["language.targetLocale"]).toBe("ja_jp");
    expect(saveSettings).not.toHaveBeenCalled();
  });

  it("翻訳先の設定が未設定なら補正値を保存する", async () => {
    vi.mocked(getSettings).mockResolvedValue({ schemaVersion: 1 });

    await useSettingsStore.getState().load();

    expect(useSettingsStore.getState().settings["language.targetLocale"]).toBe("ja_jp");
    expect(saveSettings).toHaveBeenCalledTimes(1);
    expect(saveSettings).toHaveBeenCalledWith({ "language.targetLocale": "ja_jp" });
  });

  it("補正値の保存失敗でも読み込んだ設定を使う", async () => {
    vi.mocked(getSettings).mockResolvedValue({
      schemaVersion: 1,
      "language.targetLocale": "not_a_locale",
    });
    vi.mocked(saveSettings).mockRejectedValue(new Error("write failed"));

    await useSettingsStore.getState().load();

    expect(useSettingsStore.getState().loaded).toBe(true);
    expect(useSettingsStore.getState().error).toBeNull();
    expect(useSettingsStore.getState().settings["language.targetLocale"]).toBe("ja_jp");
  });

  it("設定の読み込みに失敗した場合は保存しない", async () => {
    vi.mocked(getSettings).mockRejectedValue(new Error("read failed"));

    await useSettingsStore.getState().load();

    expect(saveSettings).not.toHaveBeenCalled();
  });
});
