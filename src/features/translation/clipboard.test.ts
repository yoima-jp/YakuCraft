import { describe, expect, it } from "vitest";

import { useEditorStore } from "../../stores/editorStore";
import type { TranslationEntry } from "../../types/translation";
import { parseTranslationClipboard, serializeTranslationClipboard } from "./clipboard";

describe("translation clipboard", () => {
  it("改行を含む訳をコピーして戻しても、後続の項目を上書きしない", () => {
    const values = ["上段\n下段", "次の項目", "保持する項目"];
    const entries: TranslationEntry[] = values.map((targetText, index) => ({
      id: index + 1,
      namespace: "sample",
      key: `message.${index + 1}`,
      sourceText: targetText,
      sourceLocale: "en_us",
      targetText,
      origin: "existing",
      flags: [],
    }));
    useEditorStore.getState().loadEntries(entries);
    const copied = serializeTranslationClipboard(values.slice(0, 2));
    useEditorStore.getState().pasteTranslations(parseTranslationClipboard(copied), 1);
    expect([...useEditorStore.getState().entries.values()].map((entry) => entry.targetText)).toEqual(values);
    expect(useEditorStore.getState().dirtyIds).toEqual([]);
  });

  it("引用符・タブ・空の末尾セルを保つ", () => {
    const values = ['"引用"\n次の行', "左右\tの語", ""];
    expect(parseTranslationClipboard(serializeTranslationClipboard(values))).toEqual(values);
  });

  it("表計算ソフトの引用されたセルとCRLFを読み取る", () => {
    expect(parseTranslationClipboard('"上段\r\n下段"\r\n"""引用"""\r\n通常')).toEqual([
      "上段\n下段", '"引用"', "通常",
    ]);
  });

  it("通常の行区切りと、引用形式ではない文章を保つ", () => {
    expect(parseTranslationClipboard('通常\r\n"未完の引用\r\n"引用"の文章\r\n')).toEqual([
      "通常", '"未完の引用', '"引用"の文章', "",
    ]);
  });
});
