import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { getAssetPreviewManifest, previewUnmounted } = vi.hoisted(() => ({
  getAssetPreviewManifest: vi.fn(),
  previewUnmounted: vi.fn(),
}));

vi.mock("../../lib/tauri", () => ({
  getAssetBytes: vi.fn(),
  getAssetPreviewManifest,
  getMinecraftJarSetting: vi.fn().mockResolvedValue(null),
}));

vi.mock("../preview/Preview", async () => {
  const { useEffect } = await import("react");
  return {
    Preview: ({ manifest }: { manifest: { manifest: { translationKey: string } } }) => {
      useEffect(() => () => previewUnmounted(), []);
      return <div data-testid="preview-manifest">{manifest.manifest.translationKey}</div>;
    },
  };
});

import { useEditorStore } from "../../stores/editorStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import type { TranslationEntry } from "../../types/translation";
import { Inspector } from "./Inspector";

const originalSettings = useSettingsStore.getState();

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

function entry(id: number, name: string): TranslationEntry {
  return {
    id,
    namespace: "example",
    key: `block.example.${name}`,
    sourceText: name,
    sourceLocale: "en_us",
    targetText: "",
    origin: "missing",
    flags: [],
  };
}

function manifest(translationKey: string) {
  return {
    kind: "texture",
    manifest: { translationKey, textures: [], candidates: [], rawModelInfo: null },
  } as const;
}

describe("Inspector preview selection", () => {
  beforeEach(() => {
    getAssetPreviewManifest.mockReset();
    previewUnmounted.mockClear();
    useEditorStore.setState({ entries: new Map([[1, entry(1, "first")], [2, entry(2, "second")]]) });
    useSettingsStore.setState({
      settings: { schemaVersion: 1, ...SETTING_DEFAULTS },
      save: async (patch) => {
        useSettingsStore.setState((state) => ({ settings: { ...state.settings, ...patch } }));
      },
    });
  });

  afterEach(() => {
    cleanup();
    useSettingsStore.setState({ settings: originalSettings.settings, save: originalSettings.save });
  });

  it("does not show a late manifest after quickly switching rows", async () => {
    const first = deferred<ReturnType<typeof manifest>>();
    const second = deferred<ReturnType<typeof manifest>>();
    getAssetPreviewManifest
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);

    const view = render(<Inspector entryId={1} open onClose={() => undefined} onTranslate={() => undefined} />);
    await waitFor(() => expect(getAssetPreviewManifest).toHaveBeenCalledWith(1, {}));

    view.rerender(<Inspector entryId={2} open onClose={() => undefined} onTranslate={() => undefined} />);
    await waitFor(() => expect(getAssetPreviewManifest).toHaveBeenCalledWith(2, {}));
    expect(screen.queryByText("block.example.first")).toBeNull();

    await act(async () => { second.resolve(manifest("block.example.second")); });
    expect((await screen.findByTestId("preview-manifest")).textContent).toBe("block.example.second");

    await act(async () => { first.resolve(manifest("block.example.first")); });
    expect(screen.getByTestId("preview-manifest").textContent).toBe("block.example.second");
    expect(screen.queryByText("block.example.first")).toBeNull();
  });

  it("非表示設定を保存し、操作可能なプレビューをunmountする", async () => {
    getAssetPreviewManifest.mockResolvedValue(manifest("block.example.first"));
    render(<Inspector entryId={1} open onClose={() => undefined} onTranslate={() => undefined} />);
    expect(await screen.findByTestId("preview-manifest")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "プレビューを非表示" }));

    await waitFor(() => {
      expect(useSettingsStore.getState().settings["editor.hideInspectorPreview"]).toBe(true);
      expect(screen.queryByTestId("preview-manifest")).toBeNull();
      expect(previewUnmounted).toHaveBeenCalledOnce();
    });
    expect(screen.getByRole("button", { name: "プレビューを表示" })).toBeTruthy();
  });
});
