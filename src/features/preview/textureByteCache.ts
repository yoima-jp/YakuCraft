import { getAssetBytes } from "../../lib/tauri";

const MAX_CACHED_BYTES = 64 * 1024 * 1024;
const cache = new Map<string, Uint8Array>();
const inFlight = new Map<string, Promise<Uint8Array>>();
let cachedByteCount = 0;
let minecraftJarGeneration = 0;

/** Texture binaryだけをサイズ上限付きLRUに置き、同じ描画中の取得も共有する。 */
export function getCachedPreviewTextureBytes(key: string, cacheScope: string): Promise<Uint8Array> {
  // sourceId/pathはMod間でも再利用されるため、呼び出し元はProjectとMod unitをscopeに含める。
  const cacheKey = `${cacheScope}\0${key}`;

  const cached = cache.get(cacheKey);
  if (cached) {
    cache.delete(cacheKey);
    cache.set(cacheKey, cached);
    return Promise.resolve(cached);
  }
  const pending = inFlight.get(cacheKey);
  if (pending) return pending;

  const usesMinecraftJar = isMinecraftJarCacheKey(cacheKey);
  const generation = minecraftJarGeneration;
  const request = loadTextureBytes(key).then((bytes) => {
    if ((!usesMinecraftJar || generation === minecraftJarGeneration) && bytes.byteLength <= MAX_CACHED_BYTES) {
      while (cachedByteCount + bytes.byteLength > MAX_CACHED_BYTES) {
        const oldest = cache.keys().next().value;
        if (oldest === undefined) break;
        const evicted = cache.get(oldest);
        if (evicted) cachedByteCount -= evicted.byteLength;
        cache.delete(oldest);
      }
      cache.set(cacheKey, bytes);
      cachedByteCount += bytes.byteLength;
    }
    return bytes;
  }).finally(() => {
    if (inFlight.get(cacheKey) === request) inFlight.delete(cacheKey);
  });
  inFlight.set(cacheKey, request);
  return request;
}

/** Minecraft JARの切替後に、同一archive pathの古いtextureを使い回さない。 */
export function invalidateMinecraftJarTextureCache(): void {
  minecraftJarGeneration += 1;
  for (const [key, bytes] of cache) {
    if (!isMinecraftJarCacheKey(key)) continue;
    cachedByteCount -= bytes.byteLength;
    cache.delete(key);
  }
  for (const key of inFlight.keys()) {
    if (isMinecraftJarCacheKey(key)) inFlight.delete(key);
  }
}

function isMinecraftJarCacheKey(cacheKey: string): boolean {
  const scopeSeparator = cacheKey.indexOf("\0");
  if (scopeSeparator < 0) return false;
  const assetKey = cacheKey.slice(scopeSeparator + 1);
  const sourceSeparator = assetKey.indexOf(":");
  if (sourceSeparator < 0) return false;
  try {
    return decodeURIComponent(assetKey.slice(0, sourceSeparator)) === "minecraft-jar";
  } catch {
    return false;
  }
}

async function loadTextureBytes(key: string): Promise<Uint8Array> {
  const separator = key.indexOf(":");
  if (separator < 0) throw new Error("Texture source is unavailable");
  const source = decodeURIComponent(key.slice(0, separator));
  const path = decodeURIComponent(key.slice(separator + 1));
  return getAssetBytes(source, path);
}
