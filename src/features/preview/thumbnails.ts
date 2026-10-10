import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";
import * as THREE from "three";

import { getAssetPreviewManifest, loadThumbnail, saveThumbnail } from "../../lib/tauri";
import type { TranslationEntry } from "../../types/translation";
import {
  classifyPreviewManifest,
  textureFetchPath,
  type EntityPreviewManifest,
  type PreviewManifest,
  type PreviewTextureLoader,
  type TextureReference,
} from "../../types/preview";
import type { AssetLocation } from "../../types/preview";
import { buildBlockPreview, buildEntityPreview, buildItemPreview, collectBlockTextures, collectModelTextures, textureReferenceFromCandidate } from "./sceneBuilder";
import { activeItemModels } from "./itemDefinitionModels";
import { disposeObjectTree } from "./geometry";
import { loadPreviewTexture } from "./textures";
import { getCachedPreviewTextureBytes } from "./textureByteCache";
import { loadMinecraftJarState, useMinecraftJarStore } from "./minecraftJarStore";

// 64pxのPNGならCreateの約1,200行をメモリに置ける一方、5万件でもLRU上限で増加を止められる。
export const THUMBNAIL_CACHE_LIMIT = 2_048;
const THUMBNAIL_RENDER_SIZE = 64;
// mcmeta取得とMod別texture cacheの修正前に生成された画像を再利用しない。
const THUMBNAIL_RESOLVER_VERSION = 3;
const THUMBNAIL_FALLBACK_DELAY_MS = 32;

export interface ThumbnailJob {
  requestKey: string;
  entry: TranslationEntry;
  projectId: string;
  jarVersion?: number;
}

export interface RenderedThumbnail {
  cacheKey: string;
  blob: Blob;
}

export type ThumbnailRenderer = (
  job: ThumbnailJob,
  isCurrent: () => boolean,
) => Promise<RenderedThumbnail | null>;

interface ThumbnailQueueOptions {
  render: ThumbnailRenderer;
  maxCacheSize?: number;
  createObjectUrl?: (blob: Blob) => string;
  revokeObjectUrl?: (url: string) => void;
}

type ScheduledThumbnailWork = { kind: "idle" | "timeout"; priority: "visible" | "background"; id: number };
type IdleCallbackWindow = Window & {
  requestIdleCallback?: (callback: () => void, options?: { timeout: number }) => number;
  cancelIdleCallback?: (id: number) => void;
};

export interface ThumbnailQueue {
  setVisible: (jobs: ThumbnailJob[]) => void;
  setBackground: (jobs: ThumbnailJob[]) => void;
  get: (requestKey: string) => string | null;
  subscribe: (requestKey: string, listener: () => void) => () => void;
  waitForIdle: () => Promise<void>;
  dispose: () => void;
  cacheSize: () => number;
}

/**
 * 可視行を先に直列処理し、残りの行はアイドル時間に1件ずつ進める。
 * rendererを行ごとに作るとWebGL上限とGPUメモリを使い切るため、実描画は後段のsingletonへ集約する。
 */
export function createThumbnailQueue({
  render,
  maxCacheSize = THUMBNAIL_CACHE_LIMIT,
  createObjectUrl = (blob) => URL.createObjectURL(blob),
  revokeObjectUrl = (url) => URL.revokeObjectURL(url),
}: ThumbnailQueueOptions): ThumbnailQueue {
  const cache = new Map<string, string>();
  const aliases = new Map<string, string>();
  const failed = new Map<string, true>();
  const listeners = new Map<string, Set<() => void>>();
  const idleListeners = new Set<() => void>();
  let visibleQueue: ThumbnailJob[] = [];
  let visibleIndex = 0;
  let visibleKeys = new Set<string>();
  let backgroundQueue: ThumbnailJob[] = [];
  let backgroundIndex = 0;
  let backgroundKeys = new Set<string>();
  let scheduled: ScheduledThumbnailWork | null = null;
  let running = false;
  let disposed = false;

  const notify = (requestKey: string) => {
    for (const listener of listeners.get(requestKey) ?? []) listener();
  };

  const readCache = (cacheKey: string) => {
    const url = cache.get(cacheKey);
    if (!url) return null;
    cache.delete(cacheKey);
    cache.set(cacheKey, url);
    return url;
  };

  const resolve = (requestKey: string) => {
    const cacheKey = aliases.get(requestKey);
    if (!cacheKey) return null;
    aliases.delete(requestKey);
    aliases.set(requestKey, cacheKey);
    const url = readCache(cacheKey);
    if (!url) aliases.delete(requestKey);
    return url;
  };

  const rememberAlias = (requestKey: string, cacheKey: string) => {
    aliases.delete(requestKey);
    aliases.set(requestKey, cacheKey);
    while (aliases.size > maxCacheSize) {
      const oldest = aliases.keys().next().value;
      if (oldest === undefined) break;
      aliases.delete(oldest);
    }
  };

  const isPending = (job: ThumbnailJob) => !resolve(job.requestKey) && !failed.has(job.requestKey);

  const nextVisible = () => {
    while (visibleIndex < visibleQueue.length) {
      const job = visibleQueue[visibleIndex];
      if (job && isPending(job)) return job;
      visibleIndex += 1;
    }
    return null;
  };

  const nextBackground = () => {
    while (backgroundIndex < backgroundQueue.length) {
      const job = backgroundQueue[backgroundIndex];
      if (job && isPending(job)) return job;
      backgroundIndex += 1;
    }
    return null;
  };

  const takeNext = () => {
    const visible = nextVisible();
    if (visible) {
      visibleIndex += 1;
      return visible;
    }
    const background = nextBackground();
    if (background) backgroundIndex += 1;
    return background;
  };

  const finishIfIdle = () => {
    if (running || scheduled || nextVisible() || nextBackground()) return;
    for (const listener of idleListeners) listener();
    idleListeners.clear();
  };

  const cancelScheduled = () => {
    if (!scheduled) return;
    if (scheduled.kind === "idle") (window as IdleCallbackWindow).cancelIdleCallback?.(scheduled.id);
    else window.clearTimeout(scheduled.id);
    scheduled = null;
  };

  const scheduleNext = () => {
    if (running || scheduled || disposed) return;
    const hasVisibleWork = nextVisible() !== null;
    if (!hasVisibleWork && !nextBackground()) {
      finishIfIdle();
      return;
    }
    const priority = hasVisibleWork ? "visible" : "background";
    const idleWindow = window as IdleCallbackWindow;
    if (idleWindow.requestIdleCallback) {
      scheduled = {
        kind: "idle",
        priority,
        id: idleWindow.requestIdleCallback(() => { void processNext(); }, { timeout: hasVisibleWork ? 100 : 500 }),
      };
    } else {
      scheduled = {
        kind: "timeout",
        priority,
        id: window.setTimeout(() => { void processNext(); }, hasVisibleWork ? 0 : THUMBNAIL_FALLBACK_DELAY_MS),
      };
    }
  };

  const processNext = async () => {
    scheduled = null;
    if (running || disposed) return;
    const job = takeNext();
    if (!job) {
      finishIfIdle();
      return;
    }
    running = true;
    const isCurrent = () => !disposed
      && (job.jarVersion === undefined || job.jarVersion === useMinecraftJarStore.getState().version)
      && (visibleKeys.has(job.requestKey) || backgroundKeys.has(job.requestKey));
    try {
      const result = await render(job, isCurrent);
      if (!result) {
        if (isCurrent()) {
          failed.delete(job.requestKey);
          failed.set(job.requestKey, true);
          while (failed.size > maxCacheSize) {
            const oldest = failed.keys().next().value;
            if (oldest === undefined) break;
            failed.delete(oldest);
          }
          notify(job.requestKey);
        }
        return;
      }
      if (!isCurrent()) return;
      let url = readCache(result.cacheKey);
      if (!url) {
        url = createObjectUrl(result.blob);
        cache.set(result.cacheKey, url);
        while (cache.size > maxCacheSize) {
          const oldest = cache.keys().next().value;
          if (oldest === undefined) break;
          const staleUrl = cache.get(oldest);
          cache.delete(oldest);
          if (staleUrl) revokeObjectUrl(staleUrl);
          for (const [requestKey, targetKey] of aliases) {
            if (targetKey === oldest) aliases.delete(requestKey);
          }
        }
      }
      rememberAlias(job.requestKey, result.cacheKey);
      failed.delete(job.requestKey);
      notify(job.requestKey);
    } catch {
      // 一時的な取得・描画エラーは「プレビューなし」と区別し、次の可視化サイクルで再試行できるよう失敗を記録しない。
      failed.delete(job.requestKey);
      if (isCurrent()) notify(job.requestKey);
    } finally {
      running = false;
      scheduleNext();
    }
  };

  return {
    setVisible: (jobs) => {
      const unique = new Map(jobs.map((job) => [job.requestKey, job]));
      visibleQueue = [...unique.values()].filter(isPending);
      visibleIndex = 0;
      visibleKeys = new Set(visibleQueue.map(({ requestKey }) => requestKey));
      if (nextVisible() && scheduled?.priority === "background") cancelScheduled();
      scheduleNext();
    },
    setBackground: (jobs) => {
      const unique = new Map(jobs.map((job) => [job.requestKey, job]));
      backgroundQueue = [...unique.values()].filter(isPending);
      backgroundIndex = 0;
      backgroundKeys = new Set(backgroundQueue.map(({ requestKey }) => requestKey));
      scheduleNext();
    },
    get: (requestKey) => resolve(requestKey),
    subscribe: (requestKey, listener) => {
      const group = listeners.get(requestKey) ?? new Set<() => void>();
      group.add(listener);
      listeners.set(requestKey, group);
      return () => {
        group.delete(listener);
        if (group.size === 0) listeners.delete(requestKey);
      };
    },
    waitForIdle: () => running || scheduled !== null || nextVisible() !== null || nextBackground() !== null
      ? new Promise((resolveIdle) => idleListeners.add(resolveIdle))
      : Promise.resolve(),
    dispose: () => {
      disposed = true;
      visibleQueue = [];
      visibleIndex = 0;
      visibleKeys.clear();
      backgroundQueue = [];
      backgroundIndex = 0;
      backgroundKeys.clear();
      cancelScheduled();
      for (const url of cache.values()) revokeObjectUrl(url);
      cache.clear();
      aliases.clear();
      failed.clear();
      listeners.clear();
      finishIfIdle();
    },
    cacheSize: () => cache.size,
  };
}

function requestKeyFor(entry: TranslationEntry, projectId: string, jarPath: string | null) {
  // JARのsourceId/pathはバージョン間で同じになる。再起動後も区別できる設定パスをディスク・メモリ双方のキーに含める。
  return JSON.stringify([projectId, THUMBNAIL_RESOLVER_VERSION, jarPath, "default", entry.id, entry.namespace, entry.key, entry.asset?.type, entry.asset?.path]);
}

const thumbnailQueue = createThumbnailQueue({ render: renderEntryThumbnail });
let visibleRequest = { entries: [] as TranslationEntry[], projectId: "" };
let backgroundRequest = { entries: [] as TranslationEntry[], projectId: "" };

function refreshThumbnailRequests() {
  const jar = useMinecraftJarStore.getState();
  const path = jar.path;
  const jobs = ({ entries, projectId }: typeof visibleRequest): ThumbnailJob[] => path === undefined
    ? []
    : entries.filter(isPreviewableEntry).map((entry) => ({
      requestKey: requestKeyFor(entry, projectId, path),
      entry, projectId, jarVersion: jar.version,
    }));
  thumbnailQueue.setVisible(jobs(visibleRequest));
  thumbnailQueue.setBackground(jobs(backgroundRequest));
}

useMinecraftJarStore.subscribe(refreshThumbnailRequests);

export function setVisiblePreviewThumbnails(entries: TranslationEntry[], projectId: string) {
  visibleRequest = { entries, projectId };
  refreshThumbnailRequests();
  void loadMinecraftJarState();
}

export function setBackgroundPreviewThumbnails(entries: TranslationEntry[], projectId: string) {
  backgroundRequest = { entries, projectId };
  refreshThumbnailRequests();
  void loadMinecraftJarState();
}

export function clearVisiblePreviewThumbnails() {
  visibleRequest = { entries: [], projectId: "" };
  thumbnailQueue.setVisible([]);
}

export function usePreviewThumbnail(entry: TranslationEntry, projectId: string) {
  const jarPath = useMinecraftJarStore((state) => state.path);
  useEffect(() => { void loadMinecraftJarState(); }, []);
  const requestKey = useMemo(() => jarPath === undefined ? "" : requestKeyFor(entry, projectId, jarPath), [entry, projectId, jarPath]);
  const subscribe = useCallback(
    (listener: () => void) => thumbnailQueue.subscribe(requestKey, listener),
    [requestKey],
  );
  const getSnapshot = useCallback(() => thumbnailQueue.get(requestKey), [requestKey]);
  return useSyncExternalStore(subscribe, getSnapshot, () => null);
}

// asset_missing / asset_ambiguous は保存時のキーとアセットの対応付けの目安でしかない。
// 例: Create の階段は blockstate・item model・inner/outer の block model が同名で並ぶため ambiguous になるが、
// resolver は blockstate から正しく1つに解決できる。表示可否は resolver の manifest（NoPreview か）で決める。
export function isPreviewableEntry(entry: TranslationEntry) {
  return ["block", "item", "entity"].some((kind) => entry.key.startsWith(`${kind}.${entry.namespace}.`));
}

async function renderEntryThumbnail(job: ThumbnailJob, isCurrent: () => boolean): Promise<RenderedThumbnail | null> {
  try {
    const cachedBytes = await loadThumbnail(job.projectId, job.requestKey);
    if (!isCurrent()) return null;
    if (cachedBytes?.length) {
      const bytes = new Uint8Array(cachedBytes);
      return {
        cacheKey: `disk:${job.requestKey}`,
        blob: new Blob([bytes.buffer as ArrayBuffer], { type: "image/png" }),
      };
    }
  } catch {
    // ディスクキャッシュは任意。読めない場合もmanifestから通常どおり生成する。
  }

  const manifest = await getAssetPreviewManifest(job.entry.id, {});
  if (!isCurrent()) return null;
  const kind = classifyPreviewManifest(manifest);
  if (kind === "unknown") return null;

  const cacheKey = manifestCacheKey(job.requestKey, manifest, kind, job.entry.key);
  const buildTextureBytes: PreviewTextureLoader = (path) => getCachedPreviewTextureBytes(path, JSON.stringify([job.projectId, job.entry.modUnitId ?? job.entry.id]));
  const build = kind === "block"
    ? buildBlockPreview(manifest.manifest as Parameters<typeof buildBlockPreview>[0], buildTextureBytes)
    : kind === "item"
      ? buildItemPreview(manifest.manifest as Parameters<typeof buildItemPreview>[0], buildTextureBytes)
      : kind === "entity" && entityCanRender3d(manifest.manifest as EntityPreviewManifest)
        ? buildEntityPreview(manifest.manifest as EntityPreviewManifest, buildTextureBytes)
        : null;

  if (build) {
    try {
      const built = await build;
      if (!isCurrent()) {
        disposeObjectTree(built.object, built.renderTargets);
        return null;
      }
      const rendered = await renderSceneThumbnail(built.object, built.renderTargets, isCurrent);
      if (rendered) {
        await persistThumbnail(job, rendered, isCurrent);
        return { cacheKey, blob: rendered };
      }
    } catch {
      // WebGLが使えない環境でも、manifestに画像があれば静止テクスチャを表示できるよう続行する。
    }
  }

  if (!isCurrent()) return null;
  const texture = await renderTextureThumbnail(manifest, kind, buildTextureBytes, isCurrent);
  if (!texture) return null;
  await persistThumbnail(job, texture, isCurrent);
  return { cacheKey, blob: texture };
}

async function persistThumbnail(job: ThumbnailJob, blob: Blob, isCurrent: () => boolean) {
  try {
    const bytes = new Uint8Array(await blob.arrayBuffer());
    if (!isCurrent()) return;
    await saveThumbnail(job.projectId, job.requestKey, Array.from(bytes));
  } catch {
    // 保存失敗で現在の表のサムネイル表示を妨げない。
  }
}

interface ThumbnailRendererState {
  renderer: THREE.WebGLRenderer;
  scene: THREE.Scene;
  camera: THREE.PerspectiveCamera;
  content: THREE.Group;
}

let sharedRenderer: ThumbnailRendererState | null = null;

function getSharedRenderer(): ThumbnailRendererState {
  if (sharedRenderer) return sharedRenderer;
  const canvas = document.createElement("canvas");
  const renderer = new THREE.WebGLRenderer({ canvas, alpha: true, antialias: false, preserveDrawingBuffer: true });
  renderer.setClearColor(0x000000, 0);
  renderer.setPixelRatio(1);
  renderer.setSize(THUMBNAIL_RENDER_SIZE, THUMBNAIL_RENDER_SIZE, false);
  const scene = new THREE.Scene();
  const content = new THREE.Group();
  scene.add(content);
  scene.add(new THREE.AmbientLight(0xffffff, 0.9));
  const keyLight = new THREE.DirectionalLight(0xffffff, 1.2);
  keyLight.position.set(-3, 6, 5);
  scene.add(keyLight);
  const fillLight = new THREE.DirectionalLight(0xffffff, 0.3);
  fillLight.position.set(4, -2, -4);
  scene.add(fillLight);
  const camera = new THREE.PerspectiveCamera(35, 1, 0.01, 100);
  sharedRenderer = { renderer, scene, camera, content };
  return sharedRenderer;
}

async function renderSceneThumbnail(
  object: THREE.Object3D,
  renderTargets: THREE.WebGLRenderTarget[],
  isCurrent: () => boolean,
): Promise<Blob | null> {
  let state: ThumbnailRendererState;
  try {
    state = getSharedRenderer();
  } catch (error) {
    disposeObjectTree(object, renderTargets);
    throw error;
  }
  state.content.add(object);
  try {
    state.content.updateMatrixWorld(true);
    const bounds = new THREE.Box3().setFromObject(state.content);
    if (bounds.isEmpty() || !isCurrent()) return null;
    const sphere = bounds.getBoundingSphere(new THREE.Sphere());
    const distance = Math.max(sphere.radius, 0.25) / Math.sin(THREE.MathUtils.degToRad(state.camera.fov / 2)) * 1.25;
    state.camera.position.copy(sphere.center).add(new THREE.Vector3(1, 0.72, 1.25).normalize().multiplyScalar(distance));
    state.camera.lookAt(sphere.center);
    state.camera.near = Math.max(distance / 100, 0.01);
    state.camera.far = Math.max(distance * 20, 100);
    state.camera.updateProjectionMatrix();
    state.renderer.render(state.scene, state.camera);
    if (!isCurrent()) return null;
    return await canvasToBlob(state.renderer.domElement);
  } finally {
    disposeObjectTree(state.content, renderTargets);
    state.content.clear();
  }
}

function canvasToBlob(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => blob ? resolve(blob) : reject(new Error("サムネイルを画像化できませんでした。")), "image/png");
  });
}

async function renderTextureThumbnail(
  manifest: PreviewManifest,
  kind: ReturnType<typeof classifyPreviewManifest>,
  getTextureBytes: PreviewTextureLoader,
  isCurrent: () => boolean,
): Promise<Blob | null> {
  const references = textureReferences(manifest, kind);
  for (const reference of references) {
    if (!isCurrent()) return null;
    if (reference.missing) continue;
    const path = textureFetchPath(reference.path, reference.location);
    const resource = await loadPreviewTexture(path, getTextureBytes);
    try {
      if (!isCurrent()) return null;
      if (resource.missing) continue;
      const canvas = document.createElement("canvas");
      canvas.width = THUMBNAIL_RENDER_SIZE;
      canvas.height = THUMBNAIL_RENDER_SIZE;
      const context = canvas.getContext("2d");
      if (!context) return null;
      const scale = Math.min(canvas.width / resource.image.width, canvas.height / resource.image.height);
      const width = resource.image.width * scale;
      const height = resource.image.height * scale;
      context.imageSmoothingEnabled = false;
      context.drawImage(resource.image, (canvas.width - width) / 2, (canvas.height - height) / 2, width, height);
      return await canvasToBlob(canvas);
    } finally {
      resource.texture.dispose();
    }
  }
  return null;
}

function textureReferences(manifest: PreviewManifest, kind: ReturnType<typeof classifyPreviewManifest>): TextureReference[] {
  const content = manifest.manifest;
  if (kind === "block") return collectBlockTextures(content as Parameters<typeof collectBlockTextures>[0]);
  if (kind === "item") {
    return activeItemModels(content as Parameters<typeof activeItemModels>[0]).flatMap(collectModelTextures);
  }
  if (kind === "entity") {
    const entity = content as EntityPreviewManifest;
    return [...(entity.texture ? [entity.texture] : []), ...entity.textureCandidates.map(textureReferenceFromCandidate)];
  }
  if (kind === "texture") {
    const textures = (content as { textures?: unknown }).textures;
    return Array.isArray(textures)
      ? textures.filter(isTextureReference)
      : [];
  }
  if (kind === "noPreview") {
    const candidates = (content as { textureCandidates?: unknown }).textureCandidates;
    return Array.isArray(candidates)
      ? candidates.filter(isTextureCandidate).map(textureReferenceFromCandidate)
      : [];
  }
  return [];
}

function isTextureReference(value: unknown): value is TextureReference {
  return Boolean(value && typeof value === "object" && "location" in value && typeof value.location === "string");
}

function isTextureCandidate(value: unknown): value is Parameters<typeof textureReferenceFromCandidate>[0] {
  if (!value || typeof value !== "object" || !("asset" in value)) return false;
  const asset = value.asset as Partial<AssetLocation> | undefined;
  return Boolean(asset && typeof asset.sourceId === "string" && typeof asset.archivePath === "string");
}

function entityCanRender3d(entity: EntityPreviewManifest) {
  return entity.geometry.bones.some((bone) =>
    bone.cubes.some((cube) => cube.uv && (cube.uv.kind === "box" || Object.keys(cube.uv.value).length > 0)),
  );
}

function manifestCacheKey(requestKey: string, manifest: PreviewManifest, kind: string, entryKey: string) {
  const content = manifest.manifest as Record<string, unknown>;
  const variants = kind === "block"
    ? [content.selectedVariant, content.selectedState]
    : kind === "item"
      ? [content.variants, content.models]
      : kind === "entity"
        ? [content.texture, content.textureCandidates]
        : kind === "texture"
          ? content.textures
          : kind === "noPreview"
            ? content.textureCandidates
            : null;
  const translationKey = typeof content.translationKey === "string" ? content.translationKey : entryKey;
  return JSON.stringify([requestKey, kind, translationKey, stableJson(variants)]);
}

function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${stableJson(record[key])}`).join(",")}}`;
  }
  return JSON.stringify(value) ?? "undefined";
}

if (typeof window !== "undefined") {
  window.addEventListener("pagehide", () => {
    thumbnailQueue.dispose();
    if (!sharedRenderer) return;
    sharedRenderer.renderer.dispose();
    sharedRenderer = null;
  }, { once: true });
}
