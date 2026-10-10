import { describe, expect, it } from "vitest";

import { isSelectedByDefault } from "./defaultSelection";

describe("isSelectedByDefault", () => {
  it("未翻訳は対象にし、部分翻訳はしきい値未満だけ対象にする", () => {
    expect(isSelectedByDefault({ kind: "untranslated" }, 100)).toBe(true);
    expect(isSelectedByDefault({ kind: "partial", coverage: 0.73 }, 100)).toBe(true);
    expect(isSelectedByDefault({ kind: "partial", coverage: 0.73 }, 73)).toBe(false);
    expect(isSelectedByDefault({ kind: "partial", coverage: 0.73 }, 72)).toBe(false);
  });

  it("翻訳済み、言語ファイルなし、読み込み失敗は対象外にする", () => {
    expect(isSelectedByDefault({ kind: "complete" }, 100)).toBe(false);
    expect(isSelectedByDefault({ kind: "no_language_files" }, 100)).toBe(false);
    expect(isSelectedByDefault({ kind: "failed", reason: "broken archive" }, 100)).toBe(false);
  });
});
