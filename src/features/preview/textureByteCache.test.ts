import { beforeEach, describe, expect, it, vi } from "vitest";
import { getAssetBytes } from "../../lib/tauri";

vi.mock("../../lib/tauri", () => ({ getAssetBytes: vi.fn() }));

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
});

describe("preview texture bytes", () => {
  it("separates both pending and cached bytes for Mods sharing a namespace and archive path", async () => {
    const { getCachedPreviewTextureBytes } = await import("./textureByteCache");
    const path = "mod-root:assets/shared/textures/item/icon.png";
    const firstScope = JSON.stringify(["project", 1]);
    const secondScope = JSON.stringify(["project", 2]);
    vi.mocked(getAssetBytes)
      .mockResolvedValueOnce(new Uint8Array([1]))
      .mockResolvedValueOnce(new Uint8Array([2]));
    const first = getCachedPreviewTextureBytes(path, firstScope);
    expect(getCachedPreviewTextureBytes(path, firstScope)).toBe(first);
    const second = getCachedPreviewTextureBytes(path, secondScope);
    expect(await first).toEqual(new Uint8Array([1]));
    expect(await second).toEqual(new Uint8Array([2]));
    expect(await getCachedPreviewTextureBytes(path, firstScope)).toEqual(new Uint8Array([1]));
    expect(await getCachedPreviewTextureBytes(path, secondScope)).toEqual(new Uint8Array([2]));
    expect(getAssetBytes).toHaveBeenCalledTimes(2);
  });

  it("fetches and caches animation metadata, including an absent optional file", async () => {
    const { getCachedPreviewTextureBytes } = await import("./textureByteCache");
    const path = "mod-root:assets/shared/textures/item/icon.png.mcmeta";
    const metadata = new TextEncoder().encode('{"animation":{"frames":[2]}}');
    vi.mocked(getAssetBytes).mockResolvedValueOnce(metadata).mockResolvedValueOnce(new Uint8Array());
    expect(await getCachedPreviewTextureBytes(path, "animated")).toEqual(metadata);
    expect(await getCachedPreviewTextureBytes(path, "static")).toEqual(new Uint8Array());
    expect(await getCachedPreviewTextureBytes(path, "static")).toEqual(new Uint8Array());
    expect(getAssetBytes).toHaveBeenCalledTimes(2);
    expect(getAssetBytes).toHaveBeenCalledWith("mod-root", "assets/shared/textures/item/icon.png.mcmeta");
  });
});
