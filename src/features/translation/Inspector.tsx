import { SecondaryButton } from "../../components/button/Buttons";
import { Tooltip } from "../../components/Controls";
import { localeDisplayName } from "../../lib/locale";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useEditorStore } from "../../stores/editorStore";
import { useSettingsStore } from "../../stores/settingsStore";
import { useProjectStore } from "../../stores/projectStore";
import { formatNumber, useT } from "../../i18n";
import { minecraftLocaleToBcp47, targetLanguageName } from "../../i18n/minecraftLanguages";
import type { TranslationEntry } from "../../types/translation";
import { EntryPreview } from "./EntryPreview";

const ORIGIN_LABELS = {
  missing: "editor.origin.missing",
  existing: "editor.origin.existing",
  ai: "editor.origin.ai",
  edited: "editor.origin.edited",
} as const;

const FLAG_LABELS = {
  source_fallback: "editor.flag.sourceFallbackNotice",
  needs_review: "editor.flag.needsReview",
  placeholder_error: "editor.flag.placeholderMismatch",
  malformed_source: "editor.flag.malformedSourceNotice",
  asset_ambiguous: "editor.flag.assetAmbiguous",
  asset_missing: "editor.flag.assetMissing",
  patchouli_text: "editor.flag.patchouliText",
} as const;

interface InspectorProps {
  entryId: number | null;
  open: boolean;
  onClose: () => void;
  onTranslate: (entryId: number) => void;
}

export function Inspector({ entryId, open, onClose, onTranslate }: InspectorProps) {
  const t = useT();
  const entry = useEditorStore((state) => (entryId === null ? undefined : state.entries.get(entryId)));
  const isShaderpack = useProjectStore((state) => state.modUnits.some((unit) => unit.id === entry?.modUnitId && unit.sourceKind === "shaderpack"));
  const hidePreview = Boolean(useSettingsStore((state) => state.settings["editor.hideInspectorPreview"] ?? false));
  const saveSettings = useSettingsStore((state) => state.save);
  const targetLocale = useProjectStore((state) => state.details?.targetLocale ?? "ja_jp");
  const targetLanguage = targetLanguageName(targetLocale);
  const targetLang = minecraftLocaleToBcp47(targetLocale);
  const className = `editor-inspector${open ? " editor-inspector--open" : ""}`;

  return (
    <aside className={className} aria-label={t("editor.inspector.ariaLabel")} hidden={!open}>
      <div className="editor-inspector__heading">
        <h2>{t("editor.inspector.title")}</h2>
        <div className="editor-inspector__heading-actions">
          {!isShaderpack && <Tooltip content={hidePreview ? t("editor.inspector.showPreview") : t("editor.inspector.hidePreview")}>
            <button
              className="editor-inspector__preview-toggle"
              type="button"
              aria-label={hidePreview ? t("editor.inspector.showPreview") : t("editor.inspector.hidePreview")}
              aria-pressed={hidePreview}
              onClick={() => void saveSettings({ "editor.hideInspectorPreview": !hidePreview }).catch(() => undefined)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6S2 12 2 12Z" />
                <circle cx="12" cy="12" r="2.5" />
                {hidePreview && <path d="m3 3 18 18" />}
              </svg>
            </button>
          </Tooltip>}
          <Tooltip content={t("editor.close")}>
            <button className="editor-inspector__close" type="button" onClick={onClose} aria-label={t("editor.inspector.close")}>
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="m6 6 12 12M18 6 6 18" />
              </svg>
            </button>
          </Tooltip>
        </div>
      </div>
      <div className="editor-inspector__scroll">
        {!open ? null : !entry ? (
          <p className="editor-inspector__empty">{t("editor.inspector.empty")}</p>
        ) : (
          <InspectorContent entry={entry} onTranslate={onTranslate} hidePreview={hidePreview} isShaderpack={isShaderpack} targetLanguage={targetLanguage} targetLang={targetLang} />
        )}
      </div>
    </aside>
  );
}

function InspectorContent({ entry, onTranslate, hidePreview, isShaderpack, targetLanguage, targetLang }: {
  entry: TranslationEntry;
  onTranslate: (entryId: number) => void;
  hidePreview: boolean;
  isShaderpack: boolean;
  targetLanguage: string;
  targetLang: string | undefined;
}) {
  const t = useT();
  const sourceLocale = languageLabel(entry.sourceLocale);
  const aiWarning = useAiJobStore((state) => state.warnings.get(entry.id));
  const aiJobActive = useAiJobStore((state) => state.activeJob !== null);
  const selectedCount = useEditorStore((state) => state.selectedIds.length);
  const glossaryMatcher = useEditorStore((state) => state.glossaryMatcher);
  const glossaryMismatches = glossaryMatcher?.mismatches(entry.sourceText, entry.targetText) ?? [];
  const aiActionLabel = selectedCount > 1
    ? t("editor.inspector.translateSelected", { count: formatNumber(selectedCount) })
    : aiWarning ? t("editor.inspector.retry") : t("editor.inspector.translateEntry");
  const warnings = [
    ...entry.flags
      .filter((flag) => flag !== "asset_missing" && flag !== "asset_ambiguous" && flag !== "patchouli_text")
      .map((flag) => t(FLAG_LABELS[flag])),
    ...(aiWarning ?? []),
    ...glossaryMismatches.map((term) => t("editor.inspector.glossaryWarning", { source: term.sourceTerm, target: term.targetTerm })),
  ];

  return (
    <>
      {!hidePreview && !isShaderpack && <div className="editor-inspector__preview-area"><EntryPreview entry={entry} /></div>}
      <div className="editor-inspector__details">
        <section className="editor-inspector__section">
          <h3>{t("editor.inspector.nameKey")}</h3>
          <p className="editor-inspector__key">{entry.key}</p>
          {!isShaderpack && <p className="editor-inspector__namespace">{entry.namespace}</p>}
        </section>
        <section className="editor-inspector__section">
          <h3>{t("editor.inspector.target", { language: targetLanguage })}</h3>
          <p className="editor-inspector__text" lang={targetLang} dir="auto">{entry.targetText || "—"}</p>
          <p className="editor-inspector__origin">{t(ORIGIN_LABELS[entry.origin])}</p>
        </section>
        <section className="editor-inspector__section">
          <h3>{t("editor.inspector.source")} <span>{sourceLocale}</span></h3>
          <p className="editor-inspector__text">{entry.sourceText || "—"}</p>
          {entry.flags.includes("source_fallback") && (
            <p className="editor-inspector__notice">{t("editor.flag.sourceFallbackNotice")}</p>
          )}
        </section>
        {warnings.length > 0 && (
          <section className="editor-inspector__section">
            <h3>{t("editor.inspector.notes")}</h3>
            <ul className="editor-inspector__warnings">
              {warnings.map((warning) => <li key={warning}>{warning}</li>)}
            </ul>
          </section>
        )}
        <section className="editor-inspector__section editor-inspector__ai">
          <h3>{t("editor.inspector.ai")}</h3>
          <SecondaryButton disabled={aiJobActive} onClick={() => onTranslate(entry.id)}>
            {aiActionLabel}
          </SecondaryButton>
        </section>
        <section className="editor-inspector__section">
          <h3>{t("editor.inspector.reference")}</h3>
          <p className="editor-inspector__path">{entry.asset?.path ?? "—"}</p>
        </section>
      </div>
    </>
  );
}

function languageLabel(locale: string | null) {
  return locale ? localeDisplayName(locale) : "—";
}
