import { SecondaryButton } from "../../components/button/Buttons";
import { Tooltip } from "../../components/Controls";
import { localeDisplayName } from "../../lib/locale";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useEditorStore } from "../../stores/editorStore";
import { useSettingsStore } from "../../stores/settingsStore";
import type { EntryFlag, TranslationEntry } from "../../types/translation";
import { EntryPreview } from "./EntryPreview";

const ORIGIN_LABELS: Record<TranslationEntry["origin"], string> = {
  missing: "未翻訳",
  existing: "既存訳",
  ai: "AI",
  edited: "編集済み",
};

const FLAG_LABELS: Record<EntryFlag, string> = {
  source_fallback: "原文を別の言語から補完",
  needs_review: "要確認",
  placeholder_error: "置換文字列が原文と一致しません",
  malformed_source: "原文の形式エラー",
  asset_ambiguous: "関連するアセット候補が複数あります",
  asset_missing: "関連するアセットがありません",
  patchouli_text: "Patchouli本文",
};

interface InspectorProps {
  entryId: number | null;
  open: boolean;
  onClose: () => void;
  onTranslate: (entryId: number) => void;
}

export function Inspector({ entryId, open, onClose, onTranslate }: InspectorProps) {
  const entry = useEditorStore((state) => (entryId === null ? undefined : state.entries.get(entryId)));
  const hidePreview = Boolean(useSettingsStore((state) => state.settings["editor.hideInspectorPreview"] ?? false));
  const saveSettings = useSettingsStore((state) => state.save);
  const className = `editor-inspector${open ? " editor-inspector--open" : ""}`;

  return (
    <aside className={className} aria-label="翻訳項目の詳細" hidden={!open}>
      <div className="editor-inspector__heading">
        <h2>詳細</h2>
        <div className="editor-inspector__heading-actions">
          <Tooltip content={hidePreview ? "プレビューを表示" : "プレビューを非表示"}>
            <button
              className="editor-inspector__preview-toggle"
              type="button"
              aria-label={hidePreview ? "プレビューを表示" : "プレビューを非表示"}
              aria-pressed={hidePreview}
              onClick={() => void saveSettings({ "editor.hideInspectorPreview": !hidePreview }).catch(() => undefined)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6S2 12 2 12Z" />
                <circle cx="12" cy="12" r="2.5" />
                {hidePreview && <path d="m3 3 18 18" />}
              </svg>
            </button>
          </Tooltip>
          <Tooltip content="閉じる">
            <button className="editor-inspector__close" type="button" onClick={onClose} aria-label="詳細を閉じる">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="m6 6 12 12M18 6 6 18" />
              </svg>
            </button>
          </Tooltip>
        </div>
      </div>
      <div className="editor-inspector__scroll">
        {!open ? null : !entry ? (
          <p className="editor-inspector__empty">項目がありません</p>
        ) : (
          <InspectorContent entry={entry} onTranslate={onTranslate} hidePreview={hidePreview} />
        )}
      </div>
    </aside>
  );
}

function InspectorContent({ entry, onTranslate, hidePreview }: {
  entry: TranslationEntry;
  onTranslate: (entryId: number) => void;
  hidePreview: boolean;
}) {
  const sourceLocale = languageLabel(entry.sourceLocale);
  const aiWarning = useAiJobStore((state) => state.warnings.get(entry.id));
  const aiJobActive = useAiJobStore((state) => state.activeJob !== null);
  const selectedCount = useEditorStore((state) => state.selectedIds.length);
  const glossaryMatcher = useEditorStore((state) => state.glossaryMatcher);
  const glossaryMismatches = glossaryMatcher?.mismatches(entry.sourceText, entry.targetText) ?? [];
  const aiActionLabel = selectedCount > 1
    ? `選択した${selectedCount.toLocaleString("ja-JP")}件を翻訳`
    : aiWarning ? "再試行" : "この項目を翻訳";
  const warnings = [
    ...entry.flags
      .filter((flag) => flag !== "asset_missing" && flag !== "asset_ambiguous" && flag !== "patchouli_text")
      .map((flag) => FLAG_LABELS[flag]),
    ...(aiWarning ?? []),
    ...glossaryMismatches.map((term) => `用語: ${term.sourceTerm} → ${term.targetTerm}`),
  ];

  return (
    <>
      {!hidePreview && <div className="editor-inspector__preview-area"><EntryPreview entry={entry} /></div>}
      <div className="editor-inspector__details">
        <section className="editor-inspector__section">
          <h3>名前 / キー</h3>
          <p className="editor-inspector__key">{entry.key}</p>
          <p className="editor-inspector__namespace">{entry.namespace}</p>
        </section>
        <section className="editor-inspector__section">
          <h3>日本語訳</h3>
          <p className="editor-inspector__text">{entry.targetText || "—"}</p>
          <p className="editor-inspector__origin">{ORIGIN_LABELS[entry.origin]}</p>
        </section>
        <section className="editor-inspector__section">
          <h3>原文 <span>{sourceLocale}</span></h3>
          <p className="editor-inspector__text">{entry.sourceText || "—"}</p>
          {entry.flags.includes("source_fallback") && (
            <p className="editor-inspector__notice">原文を別の言語から補完</p>
          )}
        </section>
        {warnings.length > 0 && (
          <section className="editor-inspector__section">
            <h3>注意点</h3>
            <ul className="editor-inspector__warnings">
              {warnings.map((warning) => <li key={warning}>{warning}</li>)}
            </ul>
          </section>
        )}
        <section className="editor-inspector__section editor-inspector__ai">
          <h3>AI</h3>
          <SecondaryButton disabled={aiJobActive} onClick={() => onTranslate(entry.id)}>
            {aiActionLabel}
          </SecondaryButton>
        </section>
        <section className="editor-inspector__section">
          <h3>参照先</h3>
          <p className="editor-inspector__path">{entry.asset?.path ?? "—"}</p>
        </section>
      </div>
    </>
  );
}

function languageLabel(locale: string | null) {
  return locale ? localeDisplayName(locale) : "—";
}
