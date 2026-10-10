import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const glossaryMocks = vi.hoisted(() => ({
  AppCommandError: class extends Error {
    constructor(readonly appError: { code: string; userMessage: string }) {
      super(appError.userMessage);
    }
  },
  deleteGlossaryTerm: vi.fn(),
  exportGlossary: vi.fn(),
  glossaryCandidates: vi.fn(),
  importGlossary: vi.fn(),
  listGlossaryTerms: vi.fn(),
  updateGlossaryTerm: vi.fn(),
  upsertGlossaryTerm: vi.fn(),
  upsertGlossaryTerms: vi.fn(),
}));

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    measure: vi.fn(),
    getTotalSize: () => count * 96,
    getVirtualItems: () => Array.from({ length: count }, (_, index) => ({
      index,
      size: 96,
      start: index * 96,
    })),
  }),
}));

vi.mock("../../lib/tauri", () => glossaryMocks);
vi.mock("../../lib/dialogs", () => ({ openFileDialog: vi.fn(), saveFileDialog: vi.fn() }));

import { useEditorStore } from "../../stores/editorStore";
import type { GlossaryTerm } from "../../types/translation";
import { GlossaryPanel } from "./GlossaryPanel";

const originalTerm: GlossaryTerm = {
  id: 7,
  sourceTerm: "Mana",
  targetTerm: "マナ",
  note: null,
  caseSensitive: false,
  createdAt: "2026-01-01T00:00:00Z",
  updatedAt: "2026-01-01T00:00:00Z",
};

function renderPanel() {
  return render(<GlossaryPanel open initialDraft={null} onClose={vi.fn()} />);
}

function termRow() {
  const sourceInput = screen.getByRole("textbox", { name: "原文 Mana" });
  const row = screen.getAllByRole("listitem").find((candidate) => candidate.contains(sourceInput));
  if (!row) throw new Error("用語行が見つかりません");
  return { row, sourceInput };
}

beforeEach(() => {
  Object.values(glossaryMocks).forEach((mock) => {
    if (typeof mock === "function" && "mockReset" in mock) mock.mockReset();
  });
  glossaryMocks.listGlossaryTerms.mockResolvedValue([originalTerm]);
  glossaryMocks.upsertGlossaryTerm.mockResolvedValue([
    originalTerm,
    { ...originalTerm, id: 8, sourceTerm: "Essence", targetTerm: "精髄" },
  ]);
  glossaryMocks.updateGlossaryTerm.mockResolvedValue([
    { ...originalTerm, sourceTerm: "Essence", targetTerm: "精髄" },
  ]);
  useEditorStore.setState({ glossaryTerms: [] });
});

afterEach(cleanup);

describe("GlossaryPanel edits", () => {
  it("原文を変えても対象項目のidで更新する", async () => {
    renderPanel();
    const { row, sourceInput } = await waitFor(() => termRow());
    fireEvent.change(sourceInput, { target: { value: "Essence" } });
    fireEvent.change(within(row).getByRole("textbox", { name: "訳語 Mana" }), { target: { value: "精髄" } });
    fireEvent.click(within(row).getByRole("button", { name: "保存" }));

    await waitFor(() => {
      expect(glossaryMocks.updateGlossaryTerm).toHaveBeenCalledWith(7, {
        sourceTerm: "Essence",
        targetTerm: "精髄",
        note: null,
        caseSensitive: false,
      });
    });
    expect(glossaryMocks.upsertGlossaryTerm).not.toHaveBeenCalled();
    expect(useEditorStore.getState().glossaryTerms).toEqual([
      { ...originalTerm, sourceTerm: "Essence", targetTerm: "精髄" },
    ]);
  });

  it("重複する原文は入力欄にエラーを示して更新しない", async () => {
    glossaryMocks.updateGlossaryTerm.mockRejectedValue(
      new glossaryMocks.AppCommandError({ code: "glossary_term_conflict", userMessage: "重複しています" }),
    );
    renderPanel();
    const { row, sourceInput } = await waitFor(() => termRow());
    fireEvent.change(sourceInput, { target: { value: "Focus" } });
    fireEvent.click(within(row).getByRole("button", { name: "保存" }));

    expect(await screen.findByText("重複しています")).toBeTruthy();
    expect(sourceInput.getAttribute("aria-invalid")).toBe("true");
    expect(glossaryMocks.upsertGlossaryTerm).not.toHaveBeenCalled();
  });
});
