import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getAssetPreviewManifest } from "../../lib/tauri";
import { useProjectStore } from "../../stores/projectStore";
import type { PreviewManifest } from "../../types/preview";
import type { ProjectDetailsDto } from "../../types/project";
import type { TranslationEntry } from "../../types/translation";
import { EntryPreview } from "./EntryPreview";
import { changeMinecraftJar, useMinecraftJarStore } from "../preview/minecraftJarStore";
import { Preview } from "../preview/Preview";
import { getCachedPreviewTextureBytes } from "../preview/textureByteCache";

vi.mock("../../lib/tauri", () => ({
  getAssetBytes: vi.fn(),
  getAssetPreviewManifest: vi.fn(),
  getMinecraftJarSetting: vi.fn().mockResolvedValue(null),
  setMinecraftJar: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../preview/Preview", () => ({ Preview: vi.fn(() => <div aria-label="3Dプレビュー" />) }));
vi.mock("../preview/textureByteCache", () => ({ getCachedPreviewTextureBytes: vi.fn(), invalidateMinecraftJarTextureCache: vi.fn() }));

const entry: TranslationEntry = {
  id: 1,
  namespace: "sample",
  key: "block.sample.machine",
  sourceText: "Machine",
  sourceLocale: "en_us",
  targetText: "機械",
  origin: "existing",
  flags: [],
};

const manifest = {
  kind: "block",
  manifest: {
    translationKey: entry.key,
    variants: [],
    selectedVariant: null,
    properties: [
      { name: "facing", values: ["north", "east"], selectedValue: "east" },
      { name: "half", values: ["bottom", "top"], selectedValue: "bottom" },
      { name: "axis", values: ["x", "y"], selectedValue: "x" },
      { name: "custom_state", values: ["special"], selectedValue: "special" },
    ],
    selectedState: { facing: "east", half: "bottom", axis: "x", custom_state: "special" },
    models: [],
  },
} as unknown as PreviewManifest;

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  useMinecraftJarStore.setState({ path: "old.jar", version: 0, error: false });
});

describe("Preview variant labels", () => {
  it("scopes texture loading to the entry's Mod unit within the same project", async () => {
    useProjectStore.setState({
      details: { project: { id: "shared-project" } } as unknown as ProjectDetailsDto,
    });
    vi.mocked(getAssetPreviewManifest).mockResolvedValue(manifest);
    const { rerender } = render(<EntryPreview entry={{ ...entry, modUnitId: 1 }} />);
    await screen.findByLabelText("3Dプレビュー");
    const firstLoader = vi.mocked(Preview).mock.calls.at(-1)![0].getTextureBytes;
    await firstLoader("mod-root:assets/shared/textures/item/icon.png");
    rerender(<EntryPreview entry={{ ...entry, id: 2, modUnitId: 2 }} />);
    await screen.findByLabelText("3Dプレビュー");
    await vi.mocked(Preview).mock.calls.at(-1)![0].getTextureBytes("mod-root:assets/shared/textures/item/icon.png");
    expect(vi.mocked(getCachedPreviewTextureBytes).mock.calls.map(([, scope]) => JSON.parse(scope)))
      .toEqual([["shared-project", 1], ["shared-project", 2]]);
  });

  it("mounted preview reloads on JAR change and unset, ignoring an old manifest response", async () => {
    const empty: PreviewManifest = { kind: "noPreview", manifest: { textureCandidates: [], rawModelInfo: null } } as PreviewManifest;
    let resolveOld!: (value: PreviewManifest) => void;
    vi.mocked(getAssetPreviewManifest)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValueOnce(manifest)
      .mockResolvedValueOnce(empty);
    render(<EntryPreview entry={entry} />);
    await waitFor(() => expect(getAssetPreviewManifest).toHaveBeenCalledTimes(1));
    await act(() => changeMinecraftJar("new.jar"));
    await screen.findByLabelText("3Dプレビュー");
    await act(async () => { resolveOld(empty); });
    expect(screen.getByLabelText("3Dプレビュー")).toBeTruthy();
    await act(() => changeMinecraftJar(null));
    expect(screen.queryByLabelText("3Dプレビュー")).toBeNull();
    await screen.findByText("プレビューなし");
    expect(getAssetPreviewManifest).toHaveBeenCalledTimes(3);
  });

  it("Minecraftのproperty名と既知の値を日本語で表示する", async () => {
    const user = userEvent.setup();
    useProjectStore.setState({
      details: { project: { id: "project-1" } } as unknown as ProjectDetailsDto,
    });
    vi.mocked(getAssetPreviewManifest).mockResolvedValue(manifest);
    render(<EntryPreview entry={entry} />);

    const facing = await screen.findByRole("combobox", { name: "向き" });
    expect(facing.textContent).toContain("東");
    await user.click(facing);
    expect(screen.getByRole("option", { name: "北" })).toBeTruthy();

    expect(screen.getByRole("combobox", { name: "上下" }).textContent).toContain("下");
    expect(screen.getByRole("combobox", { name: "軸" }).textContent).toContain("X軸");
    expect(screen.getByRole("combobox", { name: "custom_state" })).toBeTruthy();
  });
});
