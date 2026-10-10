import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Inspector } from "./Inspector";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useEditorStore } from "../../stores/editorStore";
import type { TranslationEntry } from "../../types/translation";

const entry: TranslationEntry = {
  id: 1,
  namespace: "sample",
  key: "item.apple",
  sourceText: "Apple %s",
  sourceLocale: "en_us",
  targetText: "",
  origin: "missing",
  flags: ["needs_review"],
};

describe("Inspector AI rejection", () => {
  beforeEach(() => {
    useEditorStore.getState().loadEntries([entry]);
    useAiJobStore.setState({
      activeJob: null,
      warnings: new Map([[1, ["置換文字列が原文と一致しません"]]]),
      notice: null,
    });
  });

  afterEach(() => cleanup());

  it("reject理由をWarningsに出し、再試行できる", () => {
    const retry = vi.fn();
    render(<Inspector entryId={1} open onClose={vi.fn()} onTranslate={retry} />);

    expect(screen.getByText("置換文字列が原文と一致しません")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "再試行" }));
    expect(retry).toHaveBeenCalledWith(1);
  });
});
