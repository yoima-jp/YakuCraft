import type { TranslationEntry } from "../../types/translation";

const NAMESPACES = ["yakucraft", "examplemod", "examplemod_addon", "shared"];

export function createMockEntries(count: number): TranslationEntry[] {
  return Array.from({ length: count }, (_, index) => {
    const id = index + 1;
    const hasPlaceholderError = id % 113 === 0;
    const placeholderKind = index % 53;
    const sourceText =
      hasPlaceholderError
        ? "Found %s items"
        : placeholderKind === 0
        ? "Found %s items"
        : placeholderKind === 1
          ? "Slot %1$s contains {0}"
          : placeholderKind === 2
            ? "§aLevel %d"
            : `Source text for translation key ${id.toString().padStart(5, "0")}`;
    const origin = (
      hasPlaceholderError ? "existing" : ["missing", "existing", "ai", "edited"][index % 4]
    ) as TranslationEntry["origin"];
    const targetText =
      hasPlaceholderError
        ? "アイテムを%d個発見"
        : origin === "missing"
        ? ""
        : placeholderKind === 0
          ? "アイテムを%s個発見"
          : placeholderKind === 1
            ? "スロット%1$sの内容: {0}"
            : placeholderKind === 2
              ? "§aレベル %d"
              : `翻訳キー ${id.toString().padStart(5, "0")}`;
    const flags: TranslationEntry["flags"] = [];

    if (id % 11 === 0) flags.push("needs_review");
    if (id % 17 === 0) flags.push("source_fallback");
    if (id % 113 === 0) flags.push("placeholder_error");
    if (id % 127 === 0) flags.push("asset_missing");
    if (id % 211 === 0) flags.push("asset_ambiguous");
    if (id % 251 === 0) flags.push("malformed_source");

    return {
      id,
      namespace: NAMESPACES[index % NAMESPACES.length]!,
      key: `item.examplemod.entry_${id.toString().padStart(5, "0")}`,
      sourceText,
      sourceLocale: id % 17 === 0 ? "en_gb" : "en_us",
      targetText,
      origin,
      flags,
      ...(id % 5 === 0
        ? {
            asset: {
              type: "item",
              resourceId: `examplemod:entry_${id}`,
              path: `assets/examplemod/models/item/entry_${id}.json`,
            },
          }
        : {}),
    };
  });
}
