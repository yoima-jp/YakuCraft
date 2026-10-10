import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from "../stores/appStore";
import { useSettingsStore } from "../stores/settingsStore";
import { App } from "./App";

vi.mock("../features/translation/TranslationScreen", async () => {
  const { useState } = await import("react");
  const { Dialog } = await import("../components/Controls");
  return { TranslationScreen: () => {
    const [open, setOpen] = useState(false);
    const [draft, setDraft] = useState("");
    return <section className="editor-screen">
      <button onClick={() => setOpen(true)}>用語集</button>
      <Dialog open={open} title="用語集" onClose={() => setOpen(false)}>
        <input aria-label="原文" value={draft} onChange={(event) => setDraft(event.currentTarget.value)} />
      </Dialog>
    </section>;
  } };
});
vi.mock("../features/home/HomeScreen", () => ({ HomeScreen: () => <h1>ホーム</h1> }));
vi.mock("../features/updater/updater", () => ({ checkForUpdateOnStartup: vi.fn(), useUpdaterStore: vi.fn() }));

const initialSettings = useSettingsStore.getState();

beforeEach(() => {
  useAppStore.setState({ route: "editor", settingsReturnRoute: "home" });
  useSettingsStore.setState({ ...initialSettings, loaded: true, load: vi.fn(async () => undefined) });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
});
afterEach(() => {
  cleanup();
  useSettingsStore.setState(initialSettings);
  vi.unstubAllGlobals();
});

describe("App file shortcut", () => {
  it("モーダルの未保存入力をCtrl+Oで失わない", () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "用語集" }));
    const input = screen.getByRole("textbox", { name: "原文" });
    fireEvent.change(input, { target: { value: "未保存の用語" } });
    fireEvent.keyDown(input, { key: "o", ctrlKey: true });
    expect(useAppStore.getState().route).toBe("editor");
    expect(screen.getByRole("textbox", { name: "原文" })).toHaveProperty("value", "未保存の用語");
    expect(screen.getByRole("dialog", { name: "用語集" })).toBeTruthy();
  });

  it("モーダルがなければCtrl+Oでホームに戻る", async () => {
    render(<App />);
    fireEvent.keyDown(window, { key: "o", ctrlKey: true });
    expect(await screen.findByRole("heading", { name: "ホーム" })).toBeTruthy();
  });
});
