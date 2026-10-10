import { beforeEach, describe, expect, it, vi } from "vitest";
import { getMinecraftJarSetting, setMinecraftJar } from "../../lib/tauri";
import { invalidateMinecraftJarTextureCache } from "./textureByteCache";
import { changeMinecraftJar, loadMinecraftJarState, useMinecraftJarStore } from "./minecraftJarStore";

vi.mock("../../lib/tauri", () => ({ getMinecraftJarSetting: vi.fn(), setMinecraftJar: vi.fn() }));
vi.mock("./textureByteCache", () => ({ invalidateMinecraftJarTextureCache: vi.fn() }));

beforeEach(() => {
  vi.resetAllMocks();
  useMinecraftJarStore.setState({ path: undefined, version: 0, error: false });
});

describe("Minecraft JAR preview state", () => {
  it("shares startup reads and cannot overwrite a newer setting with an in-flight read", async () => {
    let resolveRead!: (value: string | null) => void;
    vi.mocked(getMinecraftJarSetting).mockImplementation(() => new Promise((resolve) => { resolveRead = resolve; }));
    const read = loadMinecraftJarState();
    expect(loadMinecraftJarState()).toBe(read);
    await changeMinecraftJar("new.jar");
    resolveRead("old.jar");
    await read;
    expect(useMinecraftJarStore.getState().path).toBe("new.jar");
    expect(invalidateMinecraftJarTextureCache).toHaveBeenCalledTimes(1);
  });

  it("loads persisted identity, publishes unset, and preserves state on failed save", async () => {
    vi.mocked(getMinecraftJarSetting).mockResolvedValue("persisted.jar");
    await loadMinecraftJarState();
    expect(useMinecraftJarStore.getState().path).toBe("persisted.jar");
    await changeMinecraftJar(null);
    expect(useMinecraftJarStore.getState().path).toBeNull();
    const version = useMinecraftJarStore.getState().version;
    vi.mocked(setMinecraftJar).mockRejectedValue(new Error("save failed"));
    await expect(changeMinecraftJar("failed.jar")).rejects.toThrow("save failed");
    expect(useMinecraftJarStore.getState().version).toBe(version);
    expect(useMinecraftJarStore.getState().path).toBeNull();
  });
});
