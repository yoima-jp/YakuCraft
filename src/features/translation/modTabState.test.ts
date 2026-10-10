import { beforeEach, describe, expect, it } from "vitest";

import {
  modTabShortcutIndex,
  readModTabPreferences,
  reorderModTabPreferences,
  visibleModTabUnits,
  writeModTabPreferences,
} from "./modTabState";

const projectId = "project-1";
const validIds = [11, 12, 13];

describe("mod tab preferences", () => {
  beforeEach(() => localStorage.clear());

  it("保存済みの状態がない場合はすべてのmodを開く", () => {
    expect(readModTabPreferences(projectId, validIds)).toEqual({
      openIds: validIds,
      pinnedIds: [],
      activeId: 11,
    });
  });

  it("保存済みの状態を復元し、閉じたタブを開き直さない", () => {
    writeModTabPreferences(projectId, { openIds: [12], pinnedIds: [], activeId: 12 });

    expect(readModTabPreferences(projectId, validIds)).toEqual({
      openIds: [12],
      pinnedIds: [],
      activeId: 12,
    });
  });

  it("ピン留めを先にして、画面に並ぶ順にタブを返す", () => {
    const units = validIds.map((id) => ({ id }));

    expect(visibleModTabUnits(units, [13, 11], [12])).toEqual([
      { id: 12 },
      { id: 13 },
      { id: 11 },
    ]);
  });

  it("並べ替えた順序を保存し、ピン領域をまたぐ移動も表示順に反映する", () => {
    const units = validIds.map((id) => ({ id }));
    const preferences = { openIds: [11, 12, 13], pinnedIds: [12], activeId: 11 };
    const moved = reorderModTabPreferences(preferences, [12, 11, 13], 12, 2, false);

    expect(moved).toEqual({ openIds: [11, 13, 12], pinnedIds: [], activeId: 11 });
    writeModTabPreferences(projectId, moved);
    const restored = readModTabPreferences(projectId, validIds);
    expect(visibleModTabUnits(units, restored.openIds, restored.pinnedIds).map((unit) => unit.id)).toEqual([11, 13, 12]);

    const pinnedAgain = reorderModTabPreferences(
      { openIds: [11, 12, 13], pinnedIds: [12], activeId: 11 },
      [11, 12, 13],
      13,
      1,
      true,
    );
    expect(visibleModTabUnits(units, pinnedAgain.openIds, pinnedAgain.pinnedIds).map((unit) => unit.id)).toEqual([13, 12, 11]);
  });

  it("番号は1始まりで選び、9は最後の表示タブを選ぶ", () => {
    expect(modTabShortcutIndex("1", 8, 3)).toBe(0);
    expect(modTabShortcutIndex("8", 8, 3)).toBe(7);
    expect(modTabShortcutIndex("9", 8, 3)).toBe(7);
    expect(modTabShortcutIndex("8", 7, 3)).toBeNull();
  });

  it("Ctrl+Tabは表示順に循環し、逆方向も同じ順を使う", () => {
    expect(modTabShortcutIndex("Tab", 4, 3)).toBe(0);
    expect(modTabShortcutIndex("Tab", 4, 0, true)).toBe(3);
    expect(modTabShortcutIndex("Tab", 1, 0)).toBeNull();
  });
});
