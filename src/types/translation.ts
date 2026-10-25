export type TranslationOrigin = "missing" | "existing" | "ai" | "edited";
export type EntryKind = "lang" | "patchouli" | "ftbquests";

export type EntryFlag =
  | "source_fallback"
  | "needs_review"
  | "placeholder_error"
  | "malformed_source"
  | "asset_ambiguous"
  | "asset_missing"
  | "patchouli_text";

export interface AssetSummary {
  type: string;
  resourceId: string;
  path: string;
}

export interface GlossaryTerm {
  id: number;
  sourceTerm: string;
  targetTerm: string;
  note: string | null;
  caseSensitive: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface GlossaryTermInput {
  sourceTerm: string;
  targetTerm: string;
  note: string | null;
  caseSensitive: boolean;
}

export interface GlossaryCandidate {
  sourceTerm: string;
  targetTerm: string;
  count: number;
}

export interface TranslationEntry {
  id: number;
  entryKind?: EntryKind;
  /** Project由来のEntryはunitを識別できる。fixture等の単体UIデータでは未指定の場合がある。 */
  modUnitId?: number;
  namespace: string;
  key: string;
  sourceText: string | null;
  sourceLocale: string | null;
  targetText: string;
  origin: TranslationOrigin;
  flags: EntryFlag[];
  asset?: AssetSummary;
}
