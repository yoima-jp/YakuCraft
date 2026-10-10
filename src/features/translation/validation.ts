import type { EntryFlag, TranslationEntry } from "../../types/translation";

export interface PlaceholderValidator {
  hasMismatch(sourceText: string | null, targetText: string): boolean;
}

// Rust の yc-validation に置き換えるまでの仮実装。Minecraft の書式コードを含む
// 実際の文法全体は扱わず、画面上で明らかな placeholder の欠落だけを知らせる。
export const mockPlaceholderValidator: PlaceholderValidator = {
  hasMismatch(sourceText, targetText) {
    const extract = (text: string | null) =>
      [...(text ?? "").matchAll(/%\d+\$[sd]|%[sd]|\{\d+\}|§./g)].map(
        ([placeholder]) => placeholder,
      );
    const source = extract(sourceText).sort();
    const target = extract(targetText).sort();
    return source.length !== target.length || source.some((value, index) => value !== target[index]);
  },
};

export function flagsForTarget(
  entry: TranslationEntry,
  targetText: string,
  validator: PlaceholderValidator = mockPlaceholderValidator,
) {
  const flags: EntryFlag[] = entry.flags.filter((flag) => flag !== "placeholder_error");
  if (validator.hasMismatch(entry.sourceText, targetText)) {
    flags.push("placeholder_error");
  }
  return flags;
}
