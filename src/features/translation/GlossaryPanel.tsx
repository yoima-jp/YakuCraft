import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import { Dialog } from "../../components/Controls";
import { PrimaryButton, TertiaryButton } from "../../components/button/Buttons";
import { AppCommandError, deleteGlossaryTerm, exportGlossary, glossaryCandidates, importGlossary, listGlossaryTerms, updateGlossaryTerm, upsertGlossaryTerm, upsertGlossaryTerms } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { openFileDialog, saveFileDialog } from "../../lib/dialogs";
import { useEditorStore } from "../../stores/editorStore";
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
  const terms = useEditorStore((state) => state.glossaryTerms);
  const setGlossaryTerms = useEditorStore((state) => state.setGlossaryTerms);
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
    const normalizedQuery = query.trim().toLowerCase();
    return normalizedQuery
      ? terms.filter((term) => `${term.sourceTerm}\n${term.targetTerm}\n${term.note ?? ""}`.toLowerCase().includes(normalizedQuery))
      : terms;
  }, [query, terms]);
  const filteredCandidates = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return normalizedQuery
      ? candidates.filter((candidate) => `${candidate.sourceTerm}\n${candidate.targetTerm}`.toLowerCase().includes(normalizedQuery))
      : candidates;
  }, [candidates, query]);
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
        if (active) setError(errorMessage(reason, "用語集を読み込めませんでした。"));
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
      setNotice("保存しました");
      return true;
    } catch (reason: unknown) {
      if (id !== undefined && isGlossaryTermConflict(reason)) return "duplicate" as const;
      setError(errorMessage(reason, "用語を保存できませんでした。"));
      return false;
    } finally {
      setBusy(false);
    }
  }, [setGlossaryTerms]);

  const showCandidates = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setCandidates(await glossaryCandidates());
      setSelectedCandidates(new Map());
      setMode("candidates");
    } catch (reason: unknown) {
      setError(errorMessage(reason, "候補を読み込めませんでした。"));
    } finally {
      setBusy(false);
    }
  }, []);

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
      setNotice(`${inputs.length.toLocaleString("ja-JP")}件を追加しました`);
    } catch (reason: unknown) {
      setError(errorMessage(reason, "候補を追加できませんでした。"));
    } finally {
      setBusy(false);
    }
  }, [selectedCandidates, setGlossaryTerms]);

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
      setNotice("取り込みました");
    } catch (reason: unknown) {
      setError(errorMessage(reason, "用語集を取り込めませんでした。"));
    } finally {
      setBusy(false);
    }
  }, [setGlossaryTerms]);

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
      setNotice(`${count.toLocaleString("ja-JP")}件を書き出しました`);
    } catch (reason: unknown) {
      setError(errorMessage(reason, "用語集を書き出せませんでした。"));
    } finally {
      setBusy(false);
    }
  }, []);

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
    <Dialog open={open} onClose={onClose} title="用語集" closeOnBackdrop>
      <div className="glossary-panel">
        <div className="glossary-panel__toolbar">
          <button className="glossary-panel__close" type="button" onClick={onClose} aria-label="閉じる">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" /></svg>
          </button>
          <input
            type="search"
            aria-label="用語を検索"
            placeholder="検索"
            value={query}
            onChange={(event) => setQuery(event.currentTarget.value)}
          />
          <TertiaryButton disabled={busy} onClick={() => {
            if (mode === "terms") void showCandidates();
            else setMode("terms");
          }}>
            {mode === "terms" ? "候補" : "一覧"}
          </TertiaryButton>
          <TertiaryButton disabled={busy} onClick={() => void importFile()}>取込</TertiaryButton>
          <TertiaryButton disabled={busy} onClick={() => void exportFile()}>書出</TertiaryButton>
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
          <input aria-label="原文" placeholder="原文" value={sourceTerm} onChange={(event) => setSourceTerm(event.currentTarget.value)} />
          <input aria-label="訳語" placeholder="訳語" value={targetTerm} onChange={(event) => setTargetTerm(event.currentTarget.value)} />
          <input aria-label="メモ" placeholder="メモ" value={note} onChange={(event) => setNote(event.currentTarget.value)} />
          <label><input type="checkbox" checked={caseSensitive} onChange={(event) => setCaseSensitive(event.currentTarget.checked)} />大小区別</label>
          <PrimaryButton disabled={busy || !sourceTerm.trim() || !targetTerm.trim()}>追加</PrimaryButton>
        </form>
        {mode === "candidates" && (
          <div className="glossary-panel__candidate-actions">
            <span>{filteredCandidates.length.toLocaleString("ja-JP")}件</span>
            <PrimaryButton disabled={busy || selectedCandidates.size === 0} onClick={() => void addSelectedCandidates()}>
              追加 {selectedCandidates.size > 0 ? selectedCandidates.size.toLocaleString("ja-JP") : ""}
            </PrimaryButton>
          </div>
        )}
        {error && <p className="glossary-panel__message" role="alert">{error}</p>}
        {notice && <output className="glossary-panel__message">{notice}</output>}
        {visibleItems.length === 0 ? (
          <p className="glossary-panel__empty">{mode === "terms" ? "用語はありません" : "候補はありません"}</p>
        ) : (
          <div className="glossary-panel__list" ref={scrollRef} role="list" aria-label={mode === "terms" ? "用語一覧" : "候補一覧"}>
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
                        onSave={(id, input) => save(input, id)}
                        onDelete={async (id) => {
                          setBusy(true);
                          setError(null);
                          try {
                            setGlossaryTerms(await deleteGlossaryTerm(id));
                            setNotice("削除しました");
                          } catch (reason: unknown) {
                            setError(errorMessage(reason, "用語を削除できませんでした。"));
                          } finally {
                            setBusy(false);
                          }
                        }}
                      />
                    ) : (
                      <CandidateRow
                        candidate={item as GlossaryCandidate}
                        checked={selectedCandidates.has(candidateKey(item as GlossaryCandidate))}
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
  onSave,
  onDelete,
}: {
  term: GlossaryTerm;
  busy: boolean;
  onSave: (id: number, input: GlossaryTermInput) => Promise<boolean | "duplicate">;
  onDelete: (id: number) => void;
}) {
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
          aria-label={`原文 ${term.sourceTerm}`}
          aria-invalid={sourceError || undefined}
          aria-describedby={sourceError ? `glossary-term-${term.id}-source-error` : undefined}
          defaultValue={term.sourceTerm}
          onChange={() => setSourceError(false)}
        />
        {sourceError && <span id={`glossary-term-${term.id}-source-error`} className="glossary-panel__term-error" role="alert">重複しています</span>}
      </div>
      <input ref={targetInput} aria-label={`訳語 ${term.sourceTerm}`} defaultValue={term.targetTerm} />
      <input ref={noteInput} aria-label={`メモ ${term.sourceTerm}`} defaultValue={term.note ?? ""} />
      <label><input ref={caseInput} type="checkbox" defaultChecked={term.caseSensitive} />大小区別</label>
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
      >保存</TertiaryButton>
      <TertiaryButton disabled={busy} onClick={() => onDelete(term.id)} aria-label={`用語 ${term.sourceTerm} を削除`}>削除</TertiaryButton>
    </div>
  );
}

function CandidateRow({
  candidate,
  checked,
  onChange,
}: {
  candidate: GlossaryCandidate;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="glossary-panel__candidate-row">
      <input type="checkbox" checked={checked} onChange={(event) => onChange(event.currentTarget.checked)} />
      <span>{candidate.sourceTerm} → {candidate.targetTerm}</span>
      <span className="glossary-panel__candidate-count">{candidate.count.toLocaleString("ja-JP")}</span>
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
