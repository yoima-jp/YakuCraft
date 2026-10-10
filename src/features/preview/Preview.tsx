import * as THREE from "three";
import { useEffect, useMemo, useRef, useState } from "react";
import { Checkbox, Select } from "../../components/Controls";
import {
  classifyPreviewManifest,
  previewKindOrNoPreview,
  textureFetchPath,
  type PreviewManifest,
  type PreviewMode,
  type PreviewTextureLoader,
  type AssetLocation,
  type TextureCandidate,
  type TextureReference,
  type EntityPreviewManifest,
  type ItemPreviewManifest,
} from "../../types/preview";
import { buildBlockPreview, buildEntityPreview, buildItemPreview, collectBlockTextures, collectModelTextures, textureReferenceFromCandidate } from "./sceneBuilder";
import { activeItemModels } from "./itemDefinitionModels";
import { disposeObjectTree } from "./geometry";
import { resizePreviewRenderer } from "./rendererSize";
import { createMissingTexture, loadPreviewTexture, type PreviewTextureResource } from "./textures";
import "./preview.css";

interface PreviewProps {
  manifest: PreviewManifest;
  getTextureBytes: PreviewTextureLoader;
  getModelJson: (source: AssetLocation) => Promise<unknown>;
  className?: string;
}

interface ListedTexture {
  reference: TextureReference;
  candidate: boolean;
}

type CameraView = { position: THREE.Vector3; target: THREE.Vector3 };

const MODE_LABELS: Record<PreviewMode, string> = {
  "3d": "3D",
  texture: "テクスチャ",
  model: "モデル",
};

export function Preview({ manifest, getTextureBytes, getModelJson, className = "" }: PreviewProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const viewportRef = useRef<HTMLDivElement>(null);
  const rendererRef = useRef<THREE.WebGLRenderer | null>(null);
  const sceneRef = useRef<THREE.Scene | null>(null);
  const cameraRef = useRef<THREE.PerspectiveCamera | null>(null);
  const contentRootRef = useRef<THREE.Group | null>(null);
  const renderTargetsRef = useRef<THREE.WebGLRenderTarget[]>([]);
  const viewRef = useRef<CameraView | null>(null);
  const resetViewRef = useRef<CameraView | null>(null);
  const renderGeneration = useRef(0);
  const [rendererAvailable, setRendererAvailable] = useState<boolean | null>(null);
  const [modelError, setModelError] = useState(false);
  const [modeChoice, setModeChoice] = useState<{ manifest: PreviewManifest; mode: PreviewMode }>(() => ({
    manifest,
    mode: defaultMode(manifest),
  }));
  const mode = modeChoice.manifest === manifest ? modeChoice.mode : defaultMode(manifest);
  const [textureChoice, setTextureChoice] = useState<{ manifest: PreviewManifest; index: number }>(() => ({
    manifest,
    index: 0,
  }));
  const textureIndex = textureChoice.manifest === manifest ? textureChoice.index : 0;
  const changeMode = (nextMode: PreviewMode) => setModeChoice({ manifest, mode: nextMode });
  const changeTextureIndex = (index: number) => setTextureChoice({ manifest, index });
  const rawKind = classifyPreviewManifest(manifest);
  const kind = previewKindOrNoPreview(manifest);
  const content = manifest.manifest;
  const textures = useMemo(() => getListedTextures(rawKind, content), [rawKind, content]);
  const entityHasRenderableCube = kind === "entity" && entityCanRender3d(content as EntityPreviewManifest);
  const canRender3d = kind === "block" || kind === "item" || entityHasRenderableCube;
  const classNames = `preview ${className}`.trim();
  const activeTexture = textures[Math.min(textureIndex, Math.max(0, textures.length - 1))];

  useEffect(() => {
    const canvas = canvasRef.current;
    const viewport = viewportRef.current;
    if (!canvas || !viewport) return;

    let renderer: THREE.WebGLRenderer;
    let resizeObserver: ResizeObserver | undefined;
    try {
      renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true });
      renderer.outputColorSpace = THREE.SRGBColorSpace;
    } catch {
      queueMicrotask(() => setRendererAvailable(false));
      return;
    }

    const scene = new THREE.Scene();
    scene.background = new THREE.Color(
      getComputedStyle(document.documentElement).getPropertyValue("--color-preview-background").trim(),
    );
    const contentRoot = new THREE.Group();
    scene.add(contentRoot);
    scene.add(new THREE.AmbientLight(0xffffff, 0.9));
    const keyLight = new THREE.DirectionalLight(0xffffff, 1.2);
    keyLight.position.set(-3, 6, 5);
    scene.add(keyLight);
    const fillLight = new THREE.DirectionalLight(0xffffff, 0.3);
    fillLight.position.set(4, -2, -4);
    scene.add(fillLight);

    const camera = new THREE.PerspectiveCamera(35, 1, 0.01, 100);
    camera.position.set(3, 2.5, 4);
    camera.lookAt(0, 0, 0);
    rendererRef.current = renderer;
    sceneRef.current = scene;
    cameraRef.current = camera;
    contentRootRef.current = contentRoot;

    const draw = () => renderer.render(scene, camera);
    // 設定画面の背面でもSceneを保持するため、実際に適用されたテーマに背景を追従させる。
    const themeObserver = new MutationObserver(() => {
      scene.background = new THREE.Color(
        getComputedStyle(document.documentElement).getPropertyValue("--color-preview-background").trim(),
      );
      draw();
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const resize = () => {
      // min-widthを描画寸法に流用すると、狭いcanvasがCSS上で引き伸ばされて立方体も縦横に歪む。
      resizePreviewRenderer(
        renderer,
        camera,
        viewport.clientWidth,
        viewport.clientHeight,
        window.devicePixelRatio,
      );
      // aspect変更で必要な距離も変わるため、注視点と視線方向を保ったまま現在のモデルに合わせ直す。
      const currentView = viewRef.current;
      if (currentView) {
        const fittedResetView = resetViewRef.current
          ? fitCameraToContent(camera, contentRoot, resetViewRef.current)
          : null;
        const fittedCurrentView = fitCameraToContent(camera, contentRoot, currentView);
        if (fittedResetView) resetViewRef.current = fittedResetView;
        if (fittedCurrentView) viewRef.current = fittedCurrentView;
      }
      draw();
    };
    resizeObserver = new ResizeObserver(resize);
    resizeObserver.observe(viewport);
    window.addEventListener("resize", resize);
    resize();

    const fitCamera = () => {
      const fittedView = fitCameraToContent(camera, contentRoot);
      if (!fittedView) return;
      viewRef.current = fittedView;
      resetViewRef.current = fittedView;
      draw();
    };
    const resetCamera = () => {
      if (!resetViewRef.current) {
        fitCamera();
        return;
      }
      viewRef.current = {
        position: resetViewRef.current.position.clone(),
        target: resetViewRef.current.target.clone(),
      };
      camera.position.copy(resetViewRef.current.position);
      camera.lookAt(resetViewRef.current.target);
      draw();
    };
    const rotateCamera = (deltaX: number, deltaY: number) => {
      const target = viewRef.current?.target ?? new THREE.Vector3();
      const offset = camera.position.clone().sub(target);
      const spherical = new THREE.Spherical().setFromVector3(offset);
      spherical.theta -= deltaX * 0.009;
      spherical.phi = THREE.MathUtils.clamp(spherical.phi - deltaY * 0.009, 0.08, Math.PI - 0.08);
      camera.position.copy(target).add(new THREE.Vector3().setFromSpherical(spherical));
      camera.lookAt(target);
      viewRef.current = { position: camera.position.clone(), target: target.clone() };
      draw();
    };
    const zoomCamera = (delta: number) => {
      const target = viewRef.current?.target ?? new THREE.Vector3();
      const offset = camera.position.clone().sub(target);
      const spherical = new THREE.Spherical().setFromVector3(offset);
      spherical.radius = THREE.MathUtils.clamp(spherical.radius * Math.exp(delta), 0.15, 100);
      camera.position.copy(target).add(new THREE.Vector3().setFromSpherical(spherical));
      camera.lookAt(target);
      viewRef.current = { position: camera.position.clone(), target: target.clone() };
      draw();
    };

    type DragState = { pointerId: number; x: number; y: number; pan: boolean };
    let drag: DragState | null = null;
    const onPointerDown = (event: PointerEvent) => {
      if (event.button !== 0 && event.button !== 1) return;
      drag = { pointerId: event.pointerId, x: event.clientX, y: event.clientY, pan: event.button === 1 || event.shiftKey };
      canvas.setPointerCapture(event.pointerId);
    };
    const onPointerMove = (event: PointerEvent) => {
      if (!drag || drag.pointerId !== event.pointerId) return;
      const deltaX = event.clientX - drag.x;
      const deltaY = event.clientY - drag.y;
      drag.x = event.clientX;
      drag.y = event.clientY;
      if (drag.pan) {
        const target = viewRef.current?.target ?? new THREE.Vector3();
        const distance = camera.position.distanceTo(target);
        const worldPerPixel = 2 * distance * Math.tan(THREE.MathUtils.degToRad(camera.fov / 2)) / Math.max(canvas.clientHeight, 1);
        const right = new THREE.Vector3().setFromMatrixColumn(camera.matrixWorld, 0);
        const up = new THREE.Vector3().setFromMatrixColumn(camera.matrixWorld, 1);
        const translation = right.multiplyScalar(-deltaX * worldPerPixel).add(up.multiplyScalar(deltaY * worldPerPixel));
        camera.position.add(translation);
        viewRef.current = { position: camera.position.clone(), target: target.add(translation) };
        camera.lookAt(viewRef.current.target);
        draw();
      } else {
        rotateCamera(deltaX, deltaY);
      }
    };
    const onPointerEnd = (event: PointerEvent) => {
      if (drag?.pointerId === event.pointerId) drag = null;
    };
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      zoomCamera(event.deltaY * 0.001);
    };
    const onDoubleClick = () => resetCamera();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key.startsWith("Arrow")) {
        event.preventDefault();
        if (event.key === "ArrowLeft") rotateCamera(-12, 0);
        if (event.key === "ArrowRight") rotateCamera(12, 0);
        if (event.key === "ArrowUp") rotateCamera(0, -12);
        if (event.key === "ArrowDown") rotateCamera(0, 12);
      } else if (event.key === "+" || event.key === "=") {
        event.preventDefault();
        zoomCamera(-0.12);
      } else if (event.key === "-") {
        event.preventDefault();
        zoomCamera(0.12);
      }
    };
    canvas.addEventListener("pointerdown", onPointerDown);
    canvas.addEventListener("pointermove", onPointerMove);
    canvas.addEventListener("pointerup", onPointerEnd);
    canvas.addEventListener("pointercancel", onPointerEnd);
    canvas.addEventListener("wheel", onWheel, { passive: false });
    canvas.addEventListener("dblclick", onDoubleClick);
    canvas.addEventListener("keydown", onKeyDown);
    queueMicrotask(() => setRendererAvailable(true));

    // RendererとSceneはこのeffectで一度だけ作り、manifest更新時はcontentRootだけ入れ替える。
    return () => {
      renderGeneration.current += 1;
      themeObserver.disconnect();
      resizeObserver?.disconnect();
      window.removeEventListener("resize", resize);
      canvas.removeEventListener("pointerdown", onPointerDown);
      canvas.removeEventListener("pointermove", onPointerMove);
      canvas.removeEventListener("pointerup", onPointerEnd);
      canvas.removeEventListener("pointercancel", onPointerEnd);
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("dblclick", onDoubleClick);
      canvas.removeEventListener("keydown", onKeyDown);
      disposeObjectTree(contentRoot, renderTargetsRef.current);
      renderer.dispose();
      rendererRef.current = null;
      sceneRef.current = null;
      cameraRef.current = null;
      contentRootRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (!rendererRef.current || !sceneRef.current || !cameraRef.current || !contentRootRef.current) return;
    const contentRoot = contentRootRef.current;
    const generation = ++renderGeneration.current;
    disposeObjectTree(contentRoot, renderTargetsRef.current);
    renderTargetsRef.current = [];
    contentRoot.clear();
    setModelError(false);

    const build = kind === "block"
      ? buildBlockPreview(content as Parameters<typeof buildBlockPreview>[0], getTextureBytes)
      : kind === "item"
        ? buildItemPreview(content as Parameters<typeof buildItemPreview>[0], getTextureBytes)
        : kind === "entity" && entityHasRenderableCube
          ? buildEntityPreview(content as EntityPreviewManifest, getTextureBytes)
          : null;

    if (!build) {
      rendererRef.current.render(sceneRef.current, cameraRef.current);
      return () => { renderGeneration.current += 1; };
    }

    void build.then((result) => {
      if (generation !== renderGeneration.current) {
        disposeObjectTree(result.object, result.renderTargets);
        return;
      }
      contentRoot.add(result.object);
      renderTargetsRef.current = result.renderTargets;
      contentRoot.updateMatrixWorld(true);
      const bounds = new THREE.Box3().setFromObject(contentRoot);
      if (bounds.isEmpty()) {
        setModelError(true);
        rendererRef.current?.render(sceneRef.current!, cameraRef.current!);
        return;
      }
      const fittedView = fitCameraToContent(cameraRef.current!, contentRoot);
      if (!fittedView) {
        setModelError(true);
        return;
      }
      viewRef.current = fittedView;
      resetViewRef.current = fittedView;
      rendererRef.current?.render(sceneRef.current!, cameraRef.current!);
    }).catch(() => {
      if (generation === renderGeneration.current) setModelError(true);
    });

    return () => { renderGeneration.current += 1; };
  }, [content, entityHasRenderableCube, getTextureBytes, kind]);

  return (
    <section className={classNames} aria-label="アセットプレビュー">
      <div className="preview__toolbar" role="tablist" aria-label="プレビューモード">
        {(["3d", "texture", "model"] as const).map((candidateMode) => {
          const enabled = candidateMode === "3d" ? canRender3d : candidateMode === "texture" ? textures.length > 0 : true;
          return (
            <button
              key={candidateMode}
              type="button"
              role="tab"
              aria-selected={mode === candidateMode}
              disabled={!enabled}
              onClick={() => changeMode(candidateMode)}
            >
              {MODE_LABELS[candidateMode]}
            </button>
          );
        })}
      </div>

      <div className="preview__viewport" ref={viewportRef}>
        <canvas
          ref={canvasRef}
          className={`preview__canvas${mode === "3d" ? " preview__canvas--active" : ""}`}
          aria-label="3Dプレビュー。ドラッグで回転、Shiftまたは中ボタンドラッグで移動、ホイールで拡大縮小。矢印キーとプラス・マイナスでも操作できます。"
          tabIndex={mode === "3d" ? 0 : -1}
        />

        {mode === "3d" && (
          <div className="preview__layer preview__layer--active preview__fallback" aria-live="polite">
            {(rendererAvailable === false || modelError || !canRender3d) && (
              <>
                <p>3Dプレビューを表示できません</p>
                {textures.length > 0 ? (
                  <button type="button" onClick={() => changeMode("texture")}>
                    {textures[0].candidate ? "テクスチャ候補" : "テクスチャ"}
                  </button>
                ) : (
                  <strong>プレビューなし</strong>
                )}
              </>
            )}
          </div>
        )}

        {mode === "texture" && (
          <TextureView
            texture={activeTexture}
            textures={textures}
            textureIndex={textureIndex}
            onTextureIndexChange={changeTextureIndex}
            getTextureBytes={getTextureBytes}
            reason={kind === "noPreview" ? (content as { reason?: string }).reason : rawKind === "texture" || rawKind === "unknown" ? "fallback" : undefined}
          />
        )}

        {mode === "model" && <ModelView kind={rawKind} manifest={content} getModelJson={getModelJson} />}

      </div>
    </section>
  );
}

function fitCameraToContent(
  camera: THREE.PerspectiveCamera,
  content: THREE.Object3D,
  referenceView?: CameraView,
): CameraView | null {
  content.updateMatrixWorld(true);
  const bounds = new THREE.Box3().setFromObject(content);
  if (bounds.isEmpty()) return null;

  const center = referenceView?.target.clone() ?? bounds.getCenter(new THREE.Vector3());
  // resizeではユーザーの回転・パンを維持し、初回表示とダブルクリック時だけinventory角度を使う。
  const towardCamera = referenceView
    ? referenceView.position.clone().sub(referenceView.target).normalize()
    : new THREE.Vector3(1, 0.72, 1.25).normalize();
  const forward = towardCamera.clone().negate();
  const right = new THREE.Vector3().crossVectors(forward, camera.up).normalize();
  const up = new THREE.Vector3().crossVectors(right, forward).normalize();
  const verticalTangent = Math.tan(THREE.MathUtils.degToRad(camera.fov / 2));
  const horizontalTangent = verticalTangent * camera.aspect;
  let distance = 0;

  for (const x of [bounds.min.x, bounds.max.x]) {
    for (const y of [bounds.min.y, bounds.max.y]) {
      for (const z of [bounds.min.z, bounds.max.z]) {
        const offset = new THREE.Vector3(x, y, z).sub(center);
        const depth = offset.dot(towardCamera);
        distance = Math.max(
          distance,
          depth + Math.abs(offset.dot(right)) / horizontalTangent,
          depth + Math.abs(offset.dot(up)) / verticalTangent,
        );
      }
    }
  }
  distance = Math.max(distance * 1.16, 0.5);

  const position = center.clone().addScaledVector(towardCamera, distance);
  camera.position.copy(position);
  camera.lookAt(center);
  const radius = bounds.getBoundingSphere(new THREE.Sphere()).radius;
  camera.near = Math.max(distance - radius * 2, 0.01);
  camera.far = Math.max(distance + radius * 3, 100);
  camera.updateProjectionMatrix();
  return { position: camera.position.clone(), target: center };
}

function TextureView({
  texture,
  textures,
  textureIndex,
  onTextureIndexChange,
  getTextureBytes,
  reason,
}: {
  texture?: ListedTexture;
  textures: ListedTexture[];
  textureIndex: number;
  onTextureIndexChange: (index: number) => void;
  getTextureBytes: PreviewTextureLoader;
  reason?: string;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const viewRef = useRef<HTMLDivElement>(null);
  const [loadedResource, setLoadedResource] = useState<{ path: string; resource: PreviewTextureResource } | null>(null);
  const [checker, setChecker] = useState(false);
  const path = texture
    ? texture.reference.missing ? "__missing__" : textureFetchPath(texture.reference.path, texture.reference.location)
    : null;
  const resource = loadedResource?.path === path ? loadedResource.resource : null;

  useEffect(() => {
    if (!path) return;
    let active = true;
    let loaded: PreviewTextureResource | null = null;
    const load = texture?.reference.missing
      ? Promise.resolve(createMissingTexture())
      : loadPreviewTexture(path, getTextureBytes);
    void load.then((result) => {
      loaded = result;
      if (!active) {
        result.texture.dispose();
        return;
      }
      setLoadedResource({ path, resource: result });
    });
    return () => {
      active = false;
      loaded?.texture.dispose();
    };
  }, [path, texture?.reference.missing, getTextureBytes]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const view = viewRef.current;
    if (!resource || !canvas || !view) return;
    canvas.width = resource.image.width;
    canvas.height = resource.image.height;
    const context = canvas.getContext("2d");
    if (!context) return;
    context.imageSmoothingEnabled = false;
    context.drawImage(resource.image, 0, 0);
    const fitImage = () => {
      const maxWidth = Math.max(view.clientWidth - 32, 1);
      const maxHeight = Math.max(view.clientHeight - 56, 1);
      const scale = Math.max(1, Math.min(16, maxWidth / resource.image.width, maxHeight / resource.image.height));
      canvas.style.width = `${resource.image.width * scale}px`;
      canvas.style.height = `${resource.image.height * scale}px`;
    };
    fitImage();
    const observer = new ResizeObserver(fitImage);
    observer.observe(view);
    return () => observer.disconnect();
  }, [resource]);

  if (!texture) {
    return (
      <div className="preview__layer preview__layer--active preview__fallback">
        <strong>プレビューなし</strong>
        {reason && <span>{previewReasonLabel(reason)}</span>}
      </div>
    );
  }

  return (
    <div className="preview__layer preview__layer--active preview__texture-view" ref={viewRef}>
      {reason && <p className="preview__notice">3Dプレビューを表示できません</p>}
      {textures.length > 1 && (
        <div className="preview__texture-select">
          <span>{texture.candidate ? "テクスチャ候補" : "テクスチャ"}</span>
          <Select
            aria-label={texture.candidate ? "テクスチャ候補" : "テクスチャ"}
            className="preview__texture-select-control"
            options={textures.map((entry, index) => ({
              value: String(index),
              label: entry.candidate ? `候補: ${entry.reference.location}` : entry.reference.location,
            }))}
            value={String(textureIndex)}
            onChange={(value) => onTextureIndexChange(Number(value))}
          />
        </div>
      )}
      <canvas
        ref={canvasRef}
        className={`preview__texture-image${checker ? " preview__texture-image--checker" : ""}`}
        aria-label={texture.reference.location}
      />
      <Checkbox className="preview__checker-toggle" checked={checker} onChange={setChecker}>
        市松模様
      </Checkbox>
      {resource?.missing && <span className="preview__missing">テクスチャを読み込めません</span>}
      {textures.length === 1 && texture.candidate && <span className="preview__candidate">候補</span>}
    </div>
  );
}

function ModelView({ kind, manifest, getModelJson }: {
  kind: string;
  manifest: unknown;
  getModelJson: (source: AssetLocation) => Promise<unknown>;
}) {
  const record = asRecord(manifest);
  const sources = useMemo(() => getModelSources(kind, manifest), [kind, manifest]);
  const [loadedModels, setLoadedModels] = useState<{
    sources: typeof sources;
    models: Array<{ path: string; value: unknown }>;
    error: boolean;
  } | null>(null);

  useEffect(() => {
    let active = true;
    if (sources.length === 0) return () => { active = false; };
    const readableSources = sources.filter((source) => source.readable);
    if (readableSources.length === 0) return () => { active = false; };
    void Promise.all(readableSources.map(async ({ path, source }) => ({ path, value: await getModelJson(source) })))
      .then((models) => { if (active) setLoadedModels({ sources, models, error: false }); })
      .catch(() => { if (active) setLoadedModels({ sources, models: [], error: true }); });
    return () => { active = false; };
  }, [getModelJson, sources]);
  const loadMatches = loadedModels?.sources === sources;
  const rawModels = loadMatches ? loadedModels.models : [];
  const loadError = loadMatches && loadedModels.error;

  return (
    <div className="preview__layer preview__layer--active preview__model-view">
      {sources.length > 0 && (
        <dl>
          {sources.map(({ path, identifier, texturePaths }, index) => (
            <div key={`${path}-${index}`}>
              <dt>モデル</dt><dd>{kind === "entity" && ["low", "medium"].includes(String(record?.confidence)) ? `候補: ${identifier}` : identifier}</dd>
              <dt>パス</dt><dd>{path}</dd>
              {texturePaths.length > 0 && <><dt>テクスチャ</dt><dd>{texturePaths.join(", ")}</dd></>}
            </div>
          ))}
        </dl>
      )}
      {record?.reason != null && String(record.reason).length > 0 && (
        <p>モデルを表示できません。テクスチャ表示または候補を確認してください。</p>
      )}
      {rawModels.length > 0 ? (
        <pre>{rawModels.map((model) => JSON.stringify(model.value, null, 2)).join("\n\n")}</pre>
      ) : loadError ? (
        <strong>元 JSON を読み込めません</strong>
      ) : sources.some((source) => source.readable) && !loadMatches ? (
        <strong>読み込み中</strong>
      ) : sources.length > 0 ? (
        <strong>組み込みの親モデル</strong>
      ) : kind !== "unknown" && record?.rawModelInfo != null ? (
        <>
          {typeof record.rawModelPath === "string" && (
            <dl><dt>パス</dt><dd>{record.rawModelPath}</dd></dl>
          )}
          <pre>{JSON.stringify(record.rawModelInfo, null, 2)}</pre>
        </>
      ) : (
        <strong>プレビューなし</strong>
      )}
    </div>
  );
}

function previewReasonLabel(reason: string) {
  if (reason === "entity geometry and texture are unavailable") {
    return "エンティティのモデルとテクスチャがありません。";
  }
  return "対応するプレビューがありません。テクスチャ候補を確認してください。";
}

function asAssetLocation(value: unknown): AssetLocation | null {
  const source = asRecord(value);
  if (!source || typeof source.archivePath !== "string" || typeof source.sourceId !== "string" ||
    typeof source.namespace !== "string" || typeof source.resourceId !== "string") return null;
  return source as unknown as AssetLocation;
}

function getModelSources(kind: string, manifest: unknown): Array<{
  path: string;
  identifier: string;
  source: AssetLocation;
  texturePaths: string[];
  readable: boolean;
}> {
  const record = asRecord(manifest);
  if (kind === "entity") {
    const source = asAssetLocation(record?.geometrySource);
    if (!source) return [];
    return [{
      path: source.archivePath,
      identifier: String(asRecord(record?.geometry)?.identifier ?? "エンティティのジオメトリ"),
      source,
      texturePaths: asCandidates(record?.textureCandidates).map((candidate) => candidate.asset.archivePath),
      readable: source.sourceKind !== "builtin",
    }];
  }
  const models = !record ? [] : kind === "block" && Array.isArray(record.models)
    ? record.models.map((entry) => asRecord(asRecord(entry)?.model)).filter((model): model is Record<string, unknown> => model !== null)
    : kind === "item"
      ? activeItemModels(manifest as ItemPreviewManifest).map((model) => asRecord(model)).filter((model): model is Record<string, unknown> => model !== null)
      : [];
  return models.flatMap((model) => {
    const sourceChain = Array.isArray(model.sourceChain)
      ? model.sourceChain.map(asAssetLocation).filter((source): source is AssetLocation => source !== null)
      : [];
    const sources = sourceChain.length > 0 ? sourceChain : [asAssetLocation(model.source)].filter(
      (source): source is AssetLocation => source !== null,
    );
    if (sources.length === 0) return [];
    const texturePaths = new Set<string>();
    const textures = asRecord(model.textures);
    for (const value of Object.values(textures ?? {})) {
      const texture = asRecord(value);
      if (!texture) continue;
      const textureSource = asAssetLocation(texture.path);
      const path = textureSource?.archivePath ?? (typeof texture.location === "string" ? texture.location : null);
      if (path) texturePaths.add(path);
    }
    if (Array.isArray(model.elements)) {
      for (const element of model.elements) {
        const faces = asRecord(asRecord(element)?.faces);
        for (const face of Object.values(faces ?? {})) {
          const textureSource = asAssetLocation(asRecord(face)?.texturePath);
          if (textureSource) texturePaths.add(textureSource.archivePath);
        }
      }
    }
    return sources.map((source, index) => ({
      path: source.archivePath,
      identifier: index === 0 && typeof model.model === "string" ? model.model : source.resourceId,
      source,
      texturePaths: index === 0 ? [...texturePaths] : [],
      readable: source.sourceKind !== "builtin",
    }));
  });
}

function getListedTextures(kind: string, manifest: unknown): ListedTexture[] {
  const record = asRecord(manifest);
  if (!record) return [];
  let direct: TextureReference[] = [];
  let candidates: TextureCandidate[] = [];

  if (kind === "block") direct = collectBlockTextures(manifest as Parameters<typeof collectBlockTextures>[0]);
  else if (kind === "item") {
    const item = manifest as Parameters<typeof buildItemPreview>[0];
    direct = activeItemModels(item).flatMap(collectModelTextures);
  } else if (kind === "texture") {
    direct = Array.isArray(record.textures) ? record.textures.filter(isTextureReference) : [];
    candidates = asCandidates(record.candidates);
  } else if (kind === "entity") {
    direct = isTextureReference(record.texture) ? [record.texture] : [];
  } else if (kind === "noPreview") {
    candidates = asCandidates(record.textureCandidates);
  }

  const candidateByPath = new Map(candidates.map((candidate) => [
    textureFetchPath(candidate.asset, `${candidate.asset.namespace}:${candidate.asset.resourceId}`),
    candidate,
  ]));
  const listed = [
    ...direct.map((reference) => {
      const candidate = candidateByPath.get(textureFetchPath(reference.path, reference.location));
      return {
        reference,
        candidate: candidate
          ? candidate.confidence !== "high"
          : kind === "entity" && record.textureConfidence !== "high",
      };
    }),
    ...candidates.map((candidate) => ({
      reference: textureReferenceFromCandidate(candidate),
      candidate: candidate.confidence !== "high" || kind === "noPreview",
    })),
  ];
  const seen = new Set<string>();
  return listed.filter((entry) => {
    const key = textureFetchPath(entry.reference.path, entry.reference.location);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function asCandidates(value: unknown): TextureCandidate[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is TextureCandidate => {
    const candidate = asRecord(entry);
    const asset = asRecord(candidate?.asset);
    return Boolean(
      asset && typeof asset.sourceId === "string" && typeof asset.archivePath === "string" &&
      typeof asset.namespace === "string" && typeof asset.resourceId === "string" &&
      typeof candidate?.confidence === "string" && typeof candidate.confirmed === "boolean",
    );
  });
}

function isTextureReference(value: unknown): value is TextureReference {
  const record = asRecord(value);
  return Boolean(record && typeof record.location === "string" && typeof record.missing === "boolean");
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function defaultMode(manifest: PreviewManifest): PreviewMode {
  const kind = classifyPreviewManifest(manifest);
  if (kind === "block" || kind === "item") return "3d";
  if (kind === "entity" && entityCanRender3d(manifest.manifest as EntityPreviewManifest)) return "3d";
  const textures = getListedTextures(kind, manifest.manifest);
  return textures.length > 0 ? "texture" : "model";
}

function entityCanRender3d(entity: EntityPreviewManifest): boolean {
  // UVのないcubeは面を生成できないため、空の3D表示よりTexture/Modelへ案内する。
  return entity.geometry.bones.some((bone) =>
    bone.cubes.some((cube) => cube.uv && (cube.uv.kind === "box" || Object.keys(cube.uv.value).length > 0)),
  );
}
