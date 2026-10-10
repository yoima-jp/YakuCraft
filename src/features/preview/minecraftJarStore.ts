import { create } from "zustand";

import { getMinecraftJarSetting, setMinecraftJar } from "../../lib/tauri";
import { invalidateMinecraftJarTextureCache } from "./textureByteCache";

interface MinecraftJarState {
  // undefined means the persisted identity is still unknown; null means explicitly unset.
  path: string | null | undefined;
  version: number;
  error: boolean;
}

export const useMinecraftJarStore = create<MinecraftJarState>()(() => ({
  path: undefined,
  version: 0,
  error: false,
}));

let loading: Promise<void> | null = null;

export function loadMinecraftJarState(): Promise<void> {
  if (useMinecraftJarStore.getState().path !== undefined) return Promise.resolve();
  if (loading) return loading;
  const version = useMinecraftJarStore.getState().version;
  loading = getMinecraftJarSetting().then((path) => {
    // A setting read started before a successful change must not restore the old JAR.
    if (useMinecraftJarStore.getState().version === version) publish(path);
  }).catch(() => {
    if (useMinecraftJarStore.getState().version === version) {
      useMinecraftJarStore.setState({ error: true });
    }
  }).finally(() => { loading = null; });
  return loading;
}

export async function changeMinecraftJar(path: string | null): Promise<void> {
  await setMinecraftJar(path);
  publish(path);
}

function publish(path: string | null) {
  invalidateMinecraftJarTextureCache();
  useMinecraftJarStore.setState((state) => ({ path, version: state.version + 1, error: false }));
}
