import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { flushBeforeLeavingEditor } from "../../lib/navigation";
import { useEditorStore } from "../../stores/editorStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import type { TranslationEntry } from "../../types/translation";
import { createMockEntries } from "./mockData";
import { MockEditorPersistence } from "./persistence";
import { TranslationTable } from "./TranslationTable";
import { useAutosave } from "./useAutosave";

function entries(): TranslationEntry[] {
  return Array.from({ length: 200 }, (_, index) => ({
    id: index + 1,
    namespace: "sample",
    key: `item.${index + 1}`,
    sourceText: index === 99 ? "A long source column value that should expand when the heading is auto fitted ".repeat(4) : `Source ${index + 1}`,
    sourceLocale: "en_us",
    targetText: `訳${index + 1}`,
    origin: "existing",
    flags: [],
  }));
}

function mockTextareaWrapping(wrapAfter: number) {
  const measurements = ["clientWidth", "scrollWidth", "clientHeight", "scrollHeight"] as const;
  const originalDescriptors = measurements.map((name) => Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, name));
  Object.defineProperties(HTMLTextAreaElement.prototype, {
    clientWidth: { configurable: true, get: () => 248 },
    scrollWidth: { configurable: true, get: () => 248 },
    clientHeight: { configurable: true, get: () => 32 },
    scrollHeight: {
      configurable: true,
      get(this: HTMLTextAreaElement) {
        return this.value.includes("\n") || this.value.length > wrapAfter ? 64 : 32;
      },
    },
  });
  return () => measurements.forEach((name, index) => {
    const descriptor = originalDescriptors[index];
    if (descriptor) Object.defineProperty(HTMLTextAreaElement.prototype, name, descriptor);
    else Reflect.deleteProperty(HTMLTextAreaElement.prototype, name);
  });
}

function nextAnimationFrame() {
  return new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
}

function AutosaveHarness({ persistence }: { persistence: MockEditorPersistence }) {
  useAutosave(persistence);
  return null;
}

describe("TranslationTable keyboard editing", () => {
  const originalOffsetHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
  const originalOffsetWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetWidth");
  const originalClientWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
  const originalTextareaMeasurements = ["clientWidth", "scrollWidth", "clientHeight", "scrollHeight"]
    .map((name) => Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, name));
  const originalSettings = useSettingsStore.getState();
  const saveSettings = vi.fn(async (patch: Record<string, unknown>) => {
    useSettingsStore.setState((state) => ({ settings: { ...state.settings, ...patch } }));
  });

  beforeAll(() => {
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: true,
      get() {
        return this.classList.contains("editor-table__scroll") ? 520 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
      configurable: true,
      get() {
        return this.classList.contains("editor-table__scroll") ? 1024 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get() {
        return this.classList.contains("editor-table__target") ? 280 : 0;
      },
    });
    Object.defineProperties(HTMLTextAreaElement.prototype, {
      clientWidth: { configurable: true, get: () => 248 },
      scrollWidth: { configurable: true, get: () => 248 },
      clientHeight: { configurable: true, get: () => 32 },
      scrollHeight: {
        configurable: true,
        get(this: HTMLTextAreaElement) {
          return this.value.includes("\n") || this.value.length > 30 ? 64 : 32;
        },
      },
    });
  });
  afterAll(() => {
    if (originalOffsetHeight) Object.defineProperty(HTMLElement.prototype, "offsetHeight", originalOffsetHeight);
    else Reflect.deleteProperty(HTMLElement.prototype, "offsetHeight");
    if (originalOffsetWidth) Object.defineProperty(HTMLElement.prototype, "offsetWidth", originalOffsetWidth);
    else Reflect.deleteProperty(HTMLElement.prototype, "offsetWidth");
    if (originalClientWidth) Object.defineProperty(HTMLElement.prototype, "clientWidth", originalClientWidth);
    else Reflect.deleteProperty(HTMLElement.prototype, "clientWidth");
    ["clientWidth", "scrollWidth", "clientHeight", "scrollHeight"].forEach((name, index) => {
      const descriptor = originalTextareaMeasurements[index];
      if (descriptor) Object.defineProperty(HTMLTextAreaElement.prototype, name, descriptor);
      else Reflect.deleteProperty(HTMLTextAreaElement.prototype, name);
    });
  });
  beforeEach(() => {
    saveSettings.mockClear();
    useSettingsStore.setState({
      settings: { schemaVersion: 1, ...SETTING_DEFAULTS },
      save: saveSettings,
    });
    useEditorStore.getState().loadEntries(entries());
  });
  afterEach(() => {
    cleanup();
    useSettingsStore.setState({ settings: originalSettings.settings, save: originalSettings.save });
  });

  it("矢印で行を移動し、Enterで編集して確定する", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const firstTarget = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    expect(firstTarget).not.toBeNull();

    fireEvent.keyDown(firstTarget!, { key: "ArrowDown" });
    expect(useEditorStore.getState().focusedCell).toEqual({ entryId: 2, column: "target" });

    const secondTarget = document.querySelector<HTMLElement>('[data-entry-id="2"][data-column="target"]');
    fireEvent.keyDown(secondTarget!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.2 の日本語訳" });
    fireEvent.change(input, { target: { value: "確定した訳" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => {
      expect(useEditorStore.getState().entries.get(2)?.targetText).toBe("確定した訳");
    });
    expect(useEditorStore.getState().entries.get(2)?.origin).toBe("edited");
  });

  it("IME変換確定中のEnterと229キーではセル編集を閉じない", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(target!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    fireEvent.change(input, { target: { value: "変換候補" } });

    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(screen.getByRole("textbox", { name: "item.1 の日本語訳" })).toBeTruthy();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("訳1");

    fireEvent.compositionEnd(input, { data: "変換候補" });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229, isComposing: false });
    expect(screen.getByRole("textbox", { name: "item.1 の日本語訳" })).toBeTruthy();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("訳1");

    fireEvent.keyDown(input, { key: "Enter", keyCode: 13, isComposing: false });
    await waitFor(() => {
      expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("変換候補");
    });
  });

  it("IME変換中のEscapeとTabでは下書きを破棄・確定しない", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(target!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    fireEvent.change(input, { target: { value: "変換中の下書き" } });

    fireEvent.keyDown(input, { key: "Escape", isComposing: true });
    expect(screen.getByRole("textbox", { name: "item.1 の日本語訳" })).toBeTruthy();
    fireEvent.keyDown(input, { key: "Tab", keyCode: 229, isComposing: false });
    expect(screen.getByRole("textbox", { name: "item.1 の日本語訳" })).toBeTruthy();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("訳1");

    fireEvent.keyDown(input, { key: "Escape", isComposing: false });
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("訳1");
  });

  it("文字入力でも編集を始め、その文字を下書きにする", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(target!, { key: "A" });

    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    expect(input).toHaveProperty("value", "A");
    fireEvent.keyDown(input, { key: "Escape" });
  });

  it("Shiftクリックで行を範囲選択し、表示文字列は選択されない", () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const firstKey = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="key"]');
    const thirdKey = document.querySelector<HTMLElement>('[data-entry-id="3"][data-column="key"]');
    expect(firstKey).not.toBeNull();
    expect(thirdKey).not.toBeNull();

    fireEvent.click(firstKey!);
    fireEvent.click(thirdKey!, { shiftKey: true });

    expect(useEditorStore.getState().selectedIds).toEqual([1, 2, 3]);
    expect(window.getSelection()?.toString()).toBe("");
  });

  it("編集中の入力欄では文字を選択でき、Ctrl+Cはブラウザへ渡す", async () => {
    const onCopy = vi.fn();
    render(<TranslationTable onCopy={onCopy} onPaste={vi.fn()} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(target!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" }) as HTMLTextAreaElement;

    input.setSelectionRange(0, 2);
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(2);
    fireEvent.keyDown(input, { key: "c", ctrlKey: true });
    expect(onCopy).not.toHaveBeenCalled();
  });

  it("Ctrl+Cは選択行のコピー操作として処理する", () => {
    const onCopy = vi.fn();
    render(<TranslationTable onCopy={onCopy} onPaste={vi.fn()} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');

    fireEvent.keyDown(target!, { key: "c", ctrlKey: true });

    expect(onCopy).toHaveBeenCalledOnce();
  });

  it("Tabで確定して次の日本語セルへ移動する", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const firstTarget = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(firstTarget!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    fireEvent.change(input, { target: { value: "タブで確定" } });
    fireEvent.keyDown(input, { key: "Tab" });

    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("タブで確定");
    expect(useEditorStore.getState().focusedCell).toEqual({ entryId: 2, column: "target" });
  });

  it("Escapeで編集中の変更を破棄する", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const firstTarget = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(firstTarget!, { key: "F2" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    fireEvent.change(input, { target: { value: "破棄する訳" } });
    fireEvent.keyDown(input, { key: "Escape" });

    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("訳1");
    expect(useEditorStore.getState().entries.get(1)?.origin).toBe("existing");
  });

  it("画面遷移で編集中の行が外れると下書きを確定する", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const firstTarget = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(firstTarget!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    fireEvent.change(input, { target: { value: "遷移前の訳" } });

    cleanup();
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("遷移前の訳");
    expect(useEditorStore.getState().entries.get(1)?.origin).toBe("edited");
  });

  it("画面遷移前のautosave flushは編集中の下書きを保存する", async () => {
    const persistence = new MockEditorPersistence({ delayMs: 0 });
    render(
      <>
        <TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />
        <AutosaveHarness persistence={persistence} />
      </>,
    );
    const firstTarget = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(firstTarget!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    fireEvent.change(input, { target: { value: "遷移時に保存する訳" } });

    await act(async () => {
      await expect(flushBeforeLeavingEditor()).resolves.toBe(true);
    });

    expect(persistence.savedEntries.get(1)?.targetText).toBe("遷移時に保存する訳");
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("遷移時に保存する訳");
  });

  it("5万件でも可視行だけをDOMへ展開する", () => {
    useEditorStore.getState().loadEntries(createMockEntries(50_000));
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    expect(screen.getAllByRole("row").length).toBeLessThan(100);
  });

  it("列境界のドラッグ幅を保存し、再表示時に復元する", async () => {
    const { unmount } = render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const handle = screen.getByRole("separator", { name: "キー列の幅" });
    const dispatchPointer = (type: string, clientX: number) => {
      const event = new Event(type, { bubbles: true });
      Object.assign(event, { pointerId: 1, clientX });
      fireEvent(handle, event);
    };
    dispatchPointer("pointerdown", 300);
    dispatchPointer("pointermove", 340);
    dispatchPointer("pointerup", 340);

    await waitFor(() => expect(useSettingsStore.getState().settings["editor.tableColumnWidths"]).toMatchObject({ key: 260 }));
    expect(handle.getAttribute("aria-valuenow")).toBe("260");

    unmount();
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    expect(screen.getByRole("separator", { name: "キー列の幅" }).getAttribute("aria-valuenow")).toBe("260");
  });

  it("列境界のダブルクリックで内容幅へ調整し、Alt+矢印でも変更する", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const sourceHandle = screen.getByRole("separator", { name: "原文列の幅" });
    fireEvent.doubleClick(sourceHandle);
    expect(Number(sourceHandle.getAttribute("aria-valuenow"))).toBeGreaterThan(240);
    await waitFor(() => expect(saveSettings).toHaveBeenCalled());

    const keyHandle = screen.getByRole("separator", { name: "キー列の幅" });
    fireEvent.keyDown(keyHandle, { key: "ArrowRight", altKey: true });
    expect(keyHandle.getAttribute("aria-valuenow")).toBe("236");
    await waitFor(() => expect(useSettingsStore.getState().settings["editor.tableColumnWidths"]).toMatchObject({ key: 236 }));
  });

  it("長い翻訳を拡大欄で編集でき、確定すると通常のセルへ戻る", async () => {
    const originalText = "これはセルに収まらない長い文章です。".repeat(6);
    useEditorStore.getState().loadEntries([{ ...entries()[0]!, targetText: originalText, sourceText: "This is a long source sentence for comparison." }]);
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(target!, { key: "Enter" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    expect(input.closest(".editor-table__editor-overlay")).not.toBeNull();
    expect(document.querySelector(".editor-table__editor-source p")?.textContent).toBe("This is a long source sentence for comparison.");

    fireEvent.change(input, { target: { value: "編集した長文" } });
    fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe(originalText);
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("編集した長文"));
    expect(document.querySelector(".editor-table__editor-overlay")).toBeNull();
  });

  it("実際に折り返す短めの文字列は編集開始時に拡大する", async () => {
    const restoreLayout = mockTextareaWrapping(24);
    const originalText = "W".repeat(32);
    useEditorStore.getState().loadEntries([{ ...entries()[0]!, targetText: originalText }]);
    try {
      render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
      const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
      fireEvent.keyDown(target!, { key: "Enter" });
      const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
      await waitFor(() => expect(input.closest(".editor-table__editor-overlay")).not.toBeNull());
      expect(input.style.height).toBe("64px");
    } finally {
      restoreLayout();
    }
  });

  it("改行のない長文を入力すると拡大し、短文では拡大しない", async () => {
    const restoreLayout = mockTextareaWrapping(24);
    try {
      render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
      const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
      fireEvent.keyDown(target!, { key: "Enter" });
      const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
      fireEvent.change(input, { target: { value: "x".repeat(100) } });
      await waitFor(() => expect(document.querySelector(".editor-table__editor-overlay")).not.toBeNull());
      expect((screen.getByRole("textbox", { name: "item.1 の日本語訳" }) as HTMLTextAreaElement).value).not.toContain("\n");
      fireEvent.keyDown(screen.getByRole("textbox", { name: "item.1 の日本語訳" }), { key: "Escape" });

      const shortTarget = document.querySelector<HTMLElement>('[data-entry-id="2"][data-column="target"]');
      fireEvent.keyDown(shortTarget!, { key: "Enter" });
      await screen.findByRole("textbox", { name: "item.2 の日本語訳" });
      expect(document.querySelector(".editor-table__editor-overlay")).toBeNull();
    } finally {
      restoreLayout();
    }
  });

  it("長文の編集はEscapeで取り消し、フォーカスが外れると確定する", async () => {
    const originalText = "長い文章を編集するテストです。".repeat(5);
    useEditorStore.getState().loadEntries([{ ...entries()[0]!, targetText: originalText }]);
    const { rerender } = render(<TranslationTable onCopy={() => undefined} onPaste={() => undefined} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(target!, { key: "F2" });
    const input = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    await nextAnimationFrame();
    fireEvent.change(input, { target: { value: "破棄する長文" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe(originalText);
    expect(document.querySelector(".editor-table__editor-overlay")).toBeNull();
    await nextAnimationFrame();

    rerender(<TranslationTable onCopy={() => undefined} onPaste={() => undefined} />);
    const restoredTarget = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(restoredTarget!, { key: "Enter" });
    const nextInput = screen.getByRole("textbox", { name: "item.1 の日本語訳" });
    await nextAnimationFrame();
    fireEvent.change(nextInput, { target: { value: "フォーカス外で確定" } });
    fireEvent.blur(nextInput);
    await waitFor(() => expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("フォーカス外で確定"));
    expect(document.querySelector(".editor-table__editor-overlay")).toBeNull();
  });

  it("短いセルの入力が列幅を超えると編集欄を拡大する", async () => {
    render(<TranslationTable onCopy={vi.fn()} onPaste={vi.fn()} />);
    const target = document.querySelector<HTMLElement>('[data-entry-id="1"][data-column="target"]');
    fireEvent.keyDown(target!, { key: "Enter" });
    const inlineInput = await screen.findByRole("textbox", { name: "item.1 の日本語訳" });
    const longDraft = "セルの幅を超える長い文章を入力しています。".repeat(4);
    fireEvent.change(inlineInput, { target: { value: longDraft } });

    await waitFor(() => expect(document.querySelector(".editor-table__editor-overlay")).not.toBeNull());
    const expandedInput = screen.getByRole("textbox", { name: "item.1 の日本語訳" }) as HTMLTextAreaElement;
    expect(expandedInput.value).toBe(longDraft);
    fireEvent.keyDown(expandedInput, { key: "Escape" });
    expect(useEditorStore.getState().entries.get(1)?.targetText).toBe("訳1");
  });
});
