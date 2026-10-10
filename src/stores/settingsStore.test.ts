import { beforeEach, describe, expect, it, vi } from "vitest";

import { getSettings, saveSettings } from "../lib/tauri";
import { readModTabPreferences, writeModTabPreferences } from "../features/translation/modTabState";
import { SETTING_DEFAULTS, useSettingsStore } from "./settingsStore";

vi.mock("../lib/tauri", () => ({
  getSettings: vi.fn(),
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
});
