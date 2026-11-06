import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import { Dialog } from "../../components/Controls";
import { PrimaryButton, TertiaryButton } from "../../components/button/Buttons";
import { AppCommandError, deleteGlossaryTerm, exportGlossary, glossaryCandidates, importGlossary, listGlossaryTerms, updateGlossaryTerm, upsertGlossaryTerm, upsertGlossaryTerms } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { openFileDialog, saveFileDialog } from "../../lib/dialogs";
import { formatNumber, lowerText, t as translate, useT, useUiLanguage } from "../../i18n";
import { minecraftLocaleToBcp47 } from "../../i18n/minecraftLanguages";
import { useEditorStore } from "../../stores/editorStore";
import { useProjectStore } from "../../stores/projectStore";
import type { GlossaryCandidate, GlossaryTerm, GlossaryTermInput } from "../../types/translation";
import "./GlossaryPanel.css";

/* oxlint-disable jsx-a11y/prefer-tag-over-role -- Virtual rows use positioned generic containers for scroll measurement. */

export interface GlossaryDraft {
  sourceTerm: string;
  targetTerm: string;
}

interface GlossaryPanelProps {
  open: boolean;
  initialDraft: GlossaryDraft | null;
  onClose: () => void;
}

type PanelMode = "terms" | "candidates";

export function GlossaryPanel({ open, initialDraft, onClose }: GlossaryPanelProps) {
  const t = useT();
  const uiLanguage = useUiLanguage();
  const terms = useEditorStore((state) => state.glossaryTerms);
  const setGlossaryTerms = useEditorStore((state) => state.setGlossaryTerms);
  const targetLocale = useProjectStore((state) => state.details?.targetLocale ?? "ja_jp");
  const targetLang = minecraftLocaleToBcp47(targetLocale);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [mode, setMode] = useState<PanelMode>("terms");
  const [candidates, setCandidates] = useState<GlossaryCandidate[]>([]);
  const [selectedCandidates, setSelectedCandidates] = useState<Map<string, GlossaryCandidate>>(new Map());
  const [query, setQuery] = useState("");
  const [sourceTerm, setSourceTerm] = useState("");
  const [targetTerm, setTargetTerm] = useState("");
  const [note, setNote] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const filteredTerms = useMemo(() => {
    const normalizedQuery = lowerText(query.trim(), uiLanguage);
    return normalizedQuery
      ? terms.filter((term) => lowerText(`${term.sourceTerm}\n${term.targetTerm}\n${term.note ?? ""}`, uiLanguage).includes(normalizedQuery))
      : terms;
  }, [query, terms, uiLanguage]);
  const filteredCandidates = useMemo(() => {
    const normalizedQuery = lowerText(query.trim(), uiLanguage);
    return normalizedQuery
      ? candidates.filter((candidate) => lowerText(`${candidate.sourceTerm}\n${candidate.targetTerm}`, uiLanguage).includes(normalizedQuery))
      : candidates;
  }, [candidates, query, uiLanguage]);
  const visibleItems = mode === "terms" ? filteredTerms : filteredCandidates;
  // oxlint-disable-next-line react/incompatible-library -- Virtualizer reads the live scroll element during render.
  const virtualizer = useVirtualizer({
    count: visibleItems.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => mode === "terms" ? 96 : 58,
    overscan: 8,
  });

  useEffect(() => {
    if (!open) return;
    setSourceTerm(initialDraft?.sourceTerm ?? "");
    setTargetTerm(initialDraft?.targetTerm ?? "");
    setNote("");
    setCaseSensitive(false);
    setQuery("");
    setError(null);
    setNotice(null);
    let active = true;
    void listGlossaryTerms()
      .then((loaded) => {
        if (active) setGlossaryTerms(loaded);
      })
      .catch((reason: unknown) => {
        if (active) setError(errorMessage(reason, translate("glossary.loadFailed")));
      });
    return () => { active = false; };
  }, [initialDraft, open, setGlossaryTerms]);

  useEffect(() => {
    virtualizer.measure();
  }, [mode, virtualizer]);

  const save = useCallback(async (input: GlossaryTermInput, id?: number) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const savedTerms = id === undefined
        ? await upsertGlossaryTerm(input)
        : await updateGlossaryTerm(id, input);
      setGlossaryTerms(savedTerms);
      setNotice(t("glossary.saved"));
      return true;
    } catch (reason: unknown) {
      if (id !== undefined && isGlossaryTermConflict(reason)) return "duplicate" as const;
      setError(errorMessage(reason, t("glossary.saveFailed")));
      return false;
    } finally {
      setBusy(false);
    }
  }, [setGlossaryTerms, t]);

  const showCandidates = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setCandidates(await glossaryCandidates());
      setSelectedCandidates(new Map());
      setMode("candidates");
    } catch (reason: unknown) {
      setError(errorMessage(reason, t("glossary.candidatesLoadFailed")));
    } finally {
      setBusy(false);
    }
  }, [t]);

  const addSelectedCandidates = useCallback(async () => {
    if (selectedCandidates.size === 0) return;
    setBusy(true);
    setError(null);
    try {
      const inputs = [...selectedCandidates.values()].map<GlossaryTermInput>((candidate) => ({
        sourceTerm: candidate.sourceTerm,
        targetTerm: candidate.targetTerm,
        note: null,
        caseSensitive: false,
      }));
      setGlossaryTerms(await upsertGlossaryTerms(inputs));
      setMode("terms");
      const messageKey = inputs.length === 1 ? "glossary.candidatesAdded_one" : "glossary.candidatesAdded";
      setNotice(t(messageKey, { count: formatNumber(inputs.length) }));
    } catch (reason: unknown) {
      setError(errorMessage(reason, t("glossary.candidatesAddFailed")));
    } finally {
      setBusy(false);
    }
  }, [selectedCandidates, setGlossaryTerms, t]);

  const importFile = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const path = await openFileDialog({
        multiple: false,
        filters: [{ name: "CSV / TSV", extensions: ["csv", "tsv"] }],
      });
      if (typeof path !== "string") return;
      setGlossaryTerms(await importGlossary(path));
      setNotice(t("glossary.imported"));
    } catch (reason: unknown) {
      setError(errorMessage(reason, t("glossary.importFailed")));
    } finally {
      setBusy(false);
    }
  }, [setGlossaryTerms, t]);

  const exportFile = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const path = await saveFileDialog({
        defaultPath: "glossary.csv",
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (!path) return;
      const count = await exportGlossary(path);
      const messageKey = count === 1 ? "glossary.exported_one" : "glossary.exported";
      setNotice(t(messageKey, { count: formatNumber(count) }));
    } catch (reason: unknown) {
      setError(errorMessage(reason, t("glossary.exportFailed")));
    } finally {
      setBusy(false);
    }
  }, [t]);

  const toggleCandidate = useCallback((candidate: GlossaryCandidate, checked: boolean) => {
    setSelectedCandidates((current) => {
      const next = new Map(current);
      const sourceKey = candidate.sourceTerm.toLowerCase();
      for (const [key, selected] of next) {
        if (selected.sourceTerm.toLowerCase() === sourceKey) next.delete(key);
      }
      if (checked) next.set(candidateKey(candidate), candidate);
      return next;
    });
  }, []);

  return (
    <Dialog open={open} onClose={onClose} title={t("glossary.title")} closeOnBackdrop>
      <div className="glossary-panel">
        <div className="glossary-panel__toolbar">
          <button className="glossary-panel__close" type="button" onClick={onClose} aria-label={t("glossary.close")}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" /></svg>
          </button>
          <input
            type="search"
            aria-label={t("glossary.search")}
            placeholder={t("glossary.searchPlaceholder")}
            value={query}
            onChange={(event) => setQuery(event.currentTarget.value)}
          />
          <TertiaryButton disabled={busy} onClick={() => {
            if (mode === "terms") void showCandidates();
            else setMode("terms");
          }}>
            {mode === "terms" ? t("glossary.mode.candidates") : t("glossary.mode.list")}
          </TertiaryButton>
          <TertiaryButton disabled={busy} onClick={() => void importFile()}>{t("glossary.import")}</TertiaryButton>
          <TertiaryButton disabled={busy} onClick={() => void exportFile()}>{t("glossary.export")}</TertiaryButton>
        </div>
        <form className="glossary-panel__add" onSubmit={(event) => {
          event.preventDefault();
          if (!sourceTerm.trim() || !targetTerm.trim() || busy) return;
          void save({
            sourceTerm: sourceTerm.trim(),
            targetTerm: targetTerm.trim(),
            note: note.trim() || null,
            caseSensitive,
          }).then((saved) => {
            if (!saved) return;
            setSourceTerm("");
            setTargetTerm("");
            setNote("");
          });
        }}>
          <input aria-label={t("glossary.source")} placeholder={t("glossary.source")} value={sourceTerm} onChange={(event) => setSourceTerm(event.currentTarget.value)} />
          <input aria-label={t("glossary.target")} placeholder={t("glossary.target")} lang={targetLang} dir="auto" value={targetTerm} onChange={(event) => setTargetTerm(event.currentTarget.value)} />
          <input aria-label={t("glossary.note")} placeholder={t("glossary.note")} value={note} onChange={(event) => setNote(event.currentTarget.value)} />
          <label><input type="checkbox" checked={caseSensitive} onChange={(event) => setCaseSensitive(event.currentTarget.checked)} />{t("glossary.caseSensitive")}</label>
          <PrimaryButton disabled={busy || !sourceTerm.trim() || !targetTerm.trim()}>{t("glossary.add")}</PrimaryButton>
        </form>
        {mode === "candidates" && (
          <div className="glossary-panel__candidate-actions">
            <span>{t("glossary.candidateCount", { count: formatNumber(filteredCandidates.length) })}</span>
            <PrimaryButton disabled={busy || selectedCandidates.size === 0} onClick={() => void addSelectedCandidates()}>
              {selectedCandidates.size > 0
                ? t("glossary.addSelected", { count: formatNumber(selectedCandidates.size) })
                : t("glossary.addSelectedNone")}
            </PrimaryButton>
          </div>
        )}
        {error && <p className="glossary-panel__message" role="alert">{error}</p>}
        {notice && <output className="glossary-panel__message">{notice}</output>}
        {visibleItems.length === 0 ? (
          <p className="glossary-panel__empty">{mode === "terms" ? t("glossary.termsEmpty") : t("glossary.candidatesEmpty")}</p>
        ) : (
          <div className="glossary-panel__list" ref={scrollRef} role="list" aria-label={mode === "terms" ? t("glossary.termsList") : t("glossary.candidatesList")}>
            <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
              {virtualizer.getVirtualItems().map((row) => {
                const item = visibleItems[row.index];
                if (!item) return null;
                return (
                  <div
                    key={mode === "terms" ? `${(item as GlossaryTerm).id}:${(item as GlossaryTerm).updatedAt}` : candidateKey(item as GlossaryCandidate)}
                    role="listitem"
                    className="glossary-panel__row"
                    style={{
                      position: "absolute",
                      insetBlockStart: 0,
                      insetInlineStart: 0,
                      width: "100%",
                      height: row.size,
                      transform: `translateY(${row.start}px)`,
                    }}
                  >
                    {mode === "terms" ? (
                      <GlossaryTermRow
                        term={item as GlossaryTerm}
                        busy={busy}
                        targetLang={targetLang}
                        onSave={(id, input) => save(input, id)}
                        onDelete={async (id) => {
                          setBusy(true);
                          setError(null);
                          try {
                            setGlossaryTerms(await deleteGlossaryTerm(id));
                            setNotice(t("glossary.deleted"));
                          } catch (reason: unknown) {
                            setError(errorMessage(reason, t("glossary.deleteFailed")));
                          } finally {
                            setBusy(false);
                          }
                        }}
                      />
                    ) : (
                      <CandidateRow
                        candidate={item as GlossaryCandidate}
                        checked={selectedCandidates.has(candidateKey(item as GlossaryCandidate))}
                        targetLang={targetLang}
                        onChange={(checked) => toggleCandidate(item as GlossaryCandidate, checked)}
                      />
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </Dialog>
  );
}

function GlossaryTermRow({
  term,
  busy,
  targetLang,
  onSave,
  onDelete,
}: {
  term: GlossaryTerm;
  busy: boolean;
  targetLang: string | undefined;
  onSave: (id: number, input: GlossaryTermInput) => Promise<boolean | "duplicate">;
  onDelete: (id: number) => void;
}) {
  const t = useT();
  const [sourceError, setSourceError] = useState(false);
  const sourceInput = useRef<HTMLInputElement>(null);
  const targetInput = useRef<HTMLInputElement>(null);
  const noteInput = useRef<HTMLInputElement>(null);
  const caseInput = useRef<HTMLInputElement>(null);
  return (
    <div className="glossary-panel__term-row">
      <div className="glossary-panel__source-field">
        <input
          ref={sourceInput}
          aria-label={t("glossary.termSource", { term: term.sourceTerm })}
          aria-invalid={sourceError || undefined}
          aria-describedby={sourceError ? `glossary-term-${term.id}-source-error` : undefined}
          defaultValue={term.sourceTerm}
          onChange={() => setSourceError(false)}
        />
        {sourceError && <span id={`glossary-term-${term.id}-source-error`} className="glossary-panel__term-error" role="alert">{t("glossary.duplicate")}</span>}
      </div>
      <input ref={targetInput} aria-label={t("glossary.termTarget", { term: term.sourceTerm })} lang={targetLang} dir="auto" defaultValue={term.targetTerm} />
      <input ref={noteInput} aria-label={t("glossary.termNote", { term: term.sourceTerm })} defaultValue={term.note ?? ""} />
      <label><input ref={caseInput} type="checkbox" defaultChecked={term.caseSensitive} />{t("glossary.caseSensitive")}</label>
      <TertiaryButton
        disabled={busy}
        onClick={() => {
          const input = {
            sourceTerm: sourceInput.current?.value.trim() ?? "",
            targetTerm: targetInput.current?.value.trim() ?? "",
            note: noteInput.current?.value.trim() || null,
            caseSensitive: Boolean(caseInput.current?.checked),
          };
          void onSave(term.id, input).then((result) => setSourceError(result === "duplicate"));
        }}
      >{t("glossary.save")}</TertiaryButton>
      <TertiaryButton disabled={busy} onClick={() => onDelete(term.id)} aria-label={t("glossary.termDelete", { term: term.sourceTerm })}>{t("glossary.delete")}</TertiaryButton>
    </div>
  );
}

function CandidateRow({
  candidate,
  checked,
  targetLang,
  onChange,
}: {
  candidate: GlossaryCandidate;
  checked: boolean;
  targetLang: string | undefined;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="glossary-panel__candidate-row">
      <input type="checkbox" checked={checked} onChange={(event) => onChange(event.currentTarget.checked)} />
      <span>{candidate.sourceTerm} → <span lang={targetLang} dir="auto">{candidate.targetTerm}</span></span>
      <span className="glossary-panel__candidate-count">{formatNumber(candidate.count)}</span>
    </label>
  );
}

function candidateKey(candidate: GlossaryCandidate) {
  return `${candidate.sourceTerm.toLowerCase()}\u0000${candidate.targetTerm}`;
}

function errorMessage(error: unknown, fallback: string) {
  void logFrontendError(error, "glossary panel");
  return error instanceof AppCommandError ? error.message : fallback;
}

function isGlossaryTermConflict(error: unknown) {
  return error instanceof AppCommandError && error.appError.code === "glossary_term_conflict";
}
