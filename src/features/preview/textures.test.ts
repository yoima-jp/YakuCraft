import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { loadPreviewTexture } from "./textures";

const drawImage = vi.fn();

beforeEach(() => {
  drawImage.mockClear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage } as unknown as CanvasRenderingContext2D);
  vi.stubGlobal("Image", class {
    naturalWidth = 16;
    naturalHeight = 48;
    onload?: () => void;
    set src(_value: string) { queueMicrotask(() => this.onload?.()); }
  });
  vi.stubGlobal("URL", { createObjectURL: vi.fn(() => "blob:texture"), revokeObjectURL: vi.fn() });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("crops the first frame named by mcmeta instead of compressing the animation sheet", async () => {
  const loader = vi.fn(async (path: string) => path.endsWith(".mcmeta")
    ? new TextEncoder().encode('{"animation":{"frames":[2,0,1]}}')
    : new Uint8Array([1]));
  const resource = await loadPreviewTexture("mod-root:sheet.png", loader);
  expect(resource.missing).toBe(false);
  expect([resource.image.width, resource.image.height]).toEqual([16, 16]);
  expect(drawImage).toHaveBeenCalledWith(expect.anything(), 0, 32, 16, 16, 0, 0, 16, 16);
  expect(loader).toHaveBeenCalledWith("mod-root:sheet.png.mcmeta");
  resource.texture.dispose();
});

it("keeps a static texture intact when its optional mcmeta is absent", async () => {
  const resource = await loadPreviewTexture("mod-root:static.png", async (path) =>
    path.endsWith(".mcmeta") ? new Uint8Array() : new Uint8Array([1]));
  expect(resource.missing).toBe(false);
  expect([resource.image.width, resource.image.height]).toEqual([16, 48]);
  expect(drawImage).toHaveBeenCalledWith(expect.anything(), 0, 0);
  resource.texture.dispose();
});
