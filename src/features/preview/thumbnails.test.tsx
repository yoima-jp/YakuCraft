import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as THREE from "three";
import { getAssetBytes, getAssetPreviewManifest, getMinecraftJarSetting, loadThumbnail } from "../../lib/tauri";
import type { TranslationEntry } from "../../types/translation";
import { changeMinecraftJar, loadMinecraftJarState, useMinecraftJarStore } from "./minecraftJarStore";
import { clearVisiblePreviewThumbnails, setBackgroundPreviewThumbnails, setVisiblePreviewThumbnails, usePreviewThumbnail } from "./thumbnails";
import { loadPreviewTexture } from "./textures";

vi.mock("../../lib/tauri", () => ({
  getAssetBytes: vi.fn(),
  getAssetPreviewManifest: vi.fn(),
  getMinecraftJarSetting: vi.fn(),
  setMinecraftJar: vi.fn().mockResolvedValue(undefined),
  loadThumbnail: vi.fn(),
  saveThumbnail: vi.fn(),
}));
vi.mock("./textures", () => ({ loadPreviewTexture: vi.fn() }));

const entry: TranslationEntry = {
  id: 1, namespace: "minecraft", key: "block.minecraft.stone", sourceText: "Stone",
  sourceLocale: "en_us", targetText: "", origin: "missing", flags: [],
};
function Thumbnail() {
  const url = usePreviewThumbnail(entry, "project");
  return <output data-testid="thumbnail">{url ?? "pending"}</output>;
}

beforeEach(() => {
  vi.clearAllMocks();
  useMinecraftJarStore.setState({ path: undefined, version: 0, error: false });
  vi.mocked(getMinecraftJarSetting).mockResolvedValue("startup.jar");
  vi.mocked(loadThumbnail).mockResolvedValue([1]);
  vi.mocked(getAssetPreviewManifest).mockResolvedValue({ kind: "noPreview", manifest: { textureCandidates: [], rawModelInfo: null } });
  let next = 0;
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: vi.fn(() => `blob:${++next}`), revokeObjectURL: vi.fn() }));
});
afterEach(() => {
  cleanup();
  clearVisiblePreviewThumbnails();
  setBackgroundPreviewThumbnails([], "");
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("JAR thumbnail identity", () => {
  it("loads each Mod's bytes for thumbnails sharing the same namespace and texture path", async () => {
    const first = { ...entry, modUnitId: 1 };
    const second = { ...entry, id: 2, modUnitId: 2 };
    const path = "mod-root:assets/shared/textures/item/thumbnail.png";
    vi.mocked(loadThumbnail).mockResolvedValue(null);
    vi.mocked(getAssetPreviewManifest).mockResolvedValue({
      kind: "texture", manifest: { translationKey: entry.key, textures: [{
        location: "shared:item/thumbnail", path: { sourceId: "mod-root", archivePath: "assets/shared/textures/item/thumbnail.png" },
        missing: false, searchedSources: [],
      }] },
    });
    vi.mocked(getAssetBytes).mockResolvedValueOnce(new Uint8Array([1])).mockResolvedValueOnce(new Uint8Array([2]));
    const seen: number[] = [];
    const image = document.createElement("canvas");
    image.width = image.height = 16;
    vi.mocked(loadPreviewTexture).mockImplementation(async (_path, loader) => {
      seen.push((await loader(path))[0]);
      return { image, texture: new THREE.CanvasTexture(image), missing: false };
    });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage: vi.fn() } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((callback) => callback(new Blob(["png"])));
    setVisiblePreviewThumbnails([first], "mod-project");
    await waitFor(() => expect(seen).toEqual([1]));
    setVisiblePreviewThumbnails([second], "mod-project");
    await waitFor(() => expect(seen).toEqual([1, 2]));
    expect(getAssetBytes).toHaveBeenCalledTimes(2);
  });

  it("keeps generated thumbnails separate even when different JARs return identical manifests", async () => {
    vi.mocked(getMinecraftJarSetting).mockResolvedValue("generated-a.jar");
    vi.mocked(loadThumbnail).mockResolvedValue(null);
    vi.mocked(getAssetPreviewManifest).mockResolvedValue({
      kind: "texture", manifest: { translationKey: entry.key, textures: [{
        location: "minecraft:block/stone", path: { sourceId: "minecraft-jar", archivePath: "assets/minecraft/textures/block/stone.png" },
        missing: false, searchedSources: [],
      }] },
    });
    const image = document.createElement("canvas");
    image.width = image.height = 16;
    vi.mocked(loadPreviewTexture).mockImplementation(async () => ({ image, texture: new THREE.CanvasTexture(image), missing: false }));
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage: vi.fn() } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((callback) => callback(new Blob(["png"])));
    setVisiblePreviewThumbnails([entry], "project");
    render(<Thumbnail />);
    await waitFor(() => expect(screen.getByTestId("thumbnail").textContent).toBe("blob:1"));
    await act(() => changeMinecraftJar("generated-b.jar"));
    await waitFor(() => expect(screen.getByTestId("thumbnail").textContent).toBe("blob:2"));
    expect(getAssetPreviewManifest).toHaveBeenCalledTimes(2);
    expect(loadPreviewTexture).toHaveBeenCalledTimes(2);
  });

  it("waits for startup identity and changes mounted disk thumbnails on change/unset without resubmitting rows", async () => {
    let resolveSetting!: (path: string | null) => void;
    vi.mocked(getMinecraftJarSetting).mockImplementation(() => new Promise((resolve) => { resolveSetting = resolve; }));
    setVisiblePreviewThumbnails([entry], "project");
    render(<Thumbnail />);
    expect(loadThumbnail).not.toHaveBeenCalled();
    await act(async () => { resolveSetting("startup.jar"); await loadMinecraftJarState(); });
    await waitFor(() => expect(screen.getByTestId("thumbnail").textContent).toBe("blob:1"));
    await act(() => changeMinecraftJar("changed.jar"));
    expect(screen.getByTestId("thumbnail").textContent).toBe("pending");
    await waitFor(() => expect(screen.getByTestId("thumbnail").textContent).toBe("blob:2"));
    await act(() => changeMinecraftJar(null));
    await waitFor(() => expect(screen.getByTestId("thumbnail").textContent).toBe("blob:3"));
    const keys = vi.mocked(loadThumbnail).mock.calls.map(([, key]) => key);
    expect(new Set(keys).size).toBe(3);
    expect(keys.map((key) => JSON.parse(key)[2])).toEqual(["startup.jar", "changed.jar", null]);
    expect(getAssetPreviewManifest).not.toHaveBeenCalled();
  });

  it("drops an in-flight old disk thumbnail and retries a no-preview row after changing JAR", async () => {
    vi.mocked(getMinecraftJarSetting).mockResolvedValue("in-flight.jar");
    let resolveOld!: (bytes: number[] | null) => void;
    vi.mocked(loadThumbnail)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce([3]);
    setBackgroundPreviewThumbnails([entry], "project");
    render(<Thumbnail />);
    await waitFor(() => expect(loadThumbnail).toHaveBeenCalledTimes(1));
    await act(() => changeMinecraftJar("missing.jar"));
    await act(async () => { resolveOld([1]); });
    await waitFor(() => expect(getAssetPreviewManifest).toHaveBeenCalledTimes(1));
    expect(screen.getByTestId("thumbnail").textContent).toBe("pending");
    expect(URL.createObjectURL).not.toHaveBeenCalled();
    await act(() => changeMinecraftJar("available.jar"));
    await waitFor(() => expect(screen.getByTestId("thumbnail").textContent).toBe("blob:1"));
  });

  it("uses the same persisted key after a fresh module startup and a different key for another saved JAR", async () => {
    const keys: string[] = [];
    // Fresh module state simulates restart: runtime version must never be the persisted identity.
    for (const path of ["restart-a.jar", "restart-a.jar", "restart-b.jar"]) {
      vi.resetModules();
      vi.mocked(getMinecraftJarSetting).mockResolvedValue(path);
      const thumbnails = await import("./thumbnails");
      thumbnails.setVisiblePreviewThumbnails([entry], "project");
      await waitFor(() => expect(loadThumbnail).toHaveBeenCalledTimes(keys.length + 1));
      keys.push(vi.mocked(loadThumbnail).mock.calls.at(-1)![1]);
      thumbnails.clearVisiblePreviewThumbnails();
      thumbnails.setBackgroundPreviewThumbnails([], "");
    }
    expect(keys[0]).toBe(keys[1]);
    expect(keys[2]).not.toBe(keys[0]);
  });
});
