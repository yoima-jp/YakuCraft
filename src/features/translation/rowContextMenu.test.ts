import { describe, expect, it, vi } from "vitest";

import { prepareRowContextSelection } from "./rowContextMenu";

describe("prepareRowContextSelection", () => {
  it("選択外の行を右クリックしたとき、その行だけを選択する", () => {
    const selectEntry = vi.fn();
    expect(prepareRowContextSelection(4, [2, 3], selectEntry)).toEqual([4]);
    expect(selectEntry).toHaveBeenCalledWith(4, "replace");
  });

  it("選択中の行なら複数選択を維持する", () => {
    const selectEntry = vi.fn();
    expect(prepareRowContextSelection(3, [2, 3, 4], selectEntry)).toEqual([2, 3, 4]);
    expect(selectEntry).not.toHaveBeenCalled();
  });
});
