import { beforeEach, describe, expect, it } from "vitest";

import type { AppRoute } from "../app/routes";
import { useAppStore } from "./appStore";
import { useEditorStore } from "./editorStore";

beforeEach(() => {
  useAppStore.setState({
    route: "home",
    settingsReturnRoute: "home",
    settingsCategory: "general",
    exportCompletion: null,
  });
  useEditorStore.setState({
    searchQuery: "",
    selectedIds: [],
    selectedIdSet: new Set(),
  });
});

describe("settings navigation", () => {
  it.each<AppRoute>(["home", "editor", "export", "exportComplete", "review"])("%sへ戻る", async (route) => {
    useAppStore.setState({ route });

    await useAppStore.getState().openSettings();
    expect(useAppStore.getState().route).toBe("settings");
    expect(useAppStore.getState().settingsReturnRoute).toBe(route);

    await useAppStore.getState().closeSettings();
    expect(useAppStore.getState().route).toBe(route);
  });

  it("設定を開いて戻ってもエディターの選択行と検索語を保つ", async () => {
    useAppStore.setState({ route: "editor" });
    useEditorStore.setState({
      searchQuery: "block.sample.machine",
      selectedIds: [17],
      selectedIdSet: new Set([17]),
    });

    await useAppStore.getState().openSettings();
    await useAppStore.getState().closeSettings();

    expect(useEditorStore.getState().searchQuery).toBe("block.sample.machine");
    expect(useEditorStore.getState().selectedIds).toEqual([17]);
  });
});
