import * as THREE from "three";
import {
  createBlockFaceGeometry,
  createBlockstateRotationMatrix,
  createUvLockedBlockFaceGeometry,
  createEntityFaceGeometry,
  createElementRotationMatrix,
  minecraftPointToThree,
} from "./geometry";
import { createMissingTexture, loadPreviewTexture, type PreviewTextureResource } from "./textures";
import { textureFetchPath } from "../../types/preview";
import type {
  BlockPreviewManifest,
  EntityCube,
  EntityCubeUv,
  EntityFaceUv,
  EntityPreviewManifest,
  FaceName,
  ItemLayer,
  ItemPreviewManifest,
  PreviewTextureLoader,
  ResolvedBlockModel,
  ResolvedElement,
  ResolvedFace,
  ResolvedModel,
  TextureCandidate,
  TextureReference,
  Tuple3,
} from "../../types/preview";
import { activeItemModels } from "./itemDefinitionModels";
import { activeItemTintColors, DEFAULT_GRASS_TINT } from "./itemTints";

export { DEFAULT_GRASS_TINT } from "./itemTints";

const FACE_NAMES: FaceName[] = ["down", "up", "north", "south", "west", "east"];

// biome情報を含まないIRなので、plains相当の既定草色を乗算色として使う。
const DEFAULT_FOLIAGE_TINT = 0x619961;
const DEFAULT_WATER_TINT = 0x3f76e4;
const DEFAULT_REDSTONE_TINT = 0x7f0000;
const PIXEL_ALPHA_THRESHOLD = 1;

interface BuildContext {
  getTextureBytes: PreviewTextureLoader;
  textures: Map<string, Promise<PreviewTextureResource>>;
  partialAlpha: WeakMap<THREE.Texture, Promise<boolean>>;
}

export interface PreviewBuildResult {
  object: THREE.Group;
  renderTargets: THREE.WebGLRenderTarget[];
}

export function textureReferenceFromCandidate(candidate: TextureCandidate): TextureReference {
  return {
    location: `${candidate.asset.namespace}:${candidate.asset.resourceId}`,
    path: candidate.asset,
    missing: false,
    searchedSources: [candidate.asset.sourceId],
  };
}

export function collectModelTextures(model: ResolvedModel): TextureReference[] {
  return Object.values(model.textures);
}

export function collectBlockTextures(manifest: BlockPreviewManifest): TextureReference[] {
  return manifest.models.flatMap(({ model }) => collectModelTextures(model));
}

export function buildBlockPreview(
  manifest: BlockPreviewManifest,
  getTextureBytes: PreviewTextureLoader,
): Promise<PreviewBuildResult> {
  const context: BuildContext = { getTextureBytes, textures: new Map(), partialAlpha: new WeakMap() };
  const root = new THREE.Group();

  return Promise.all(manifest.models.map((blockModel) => buildBlockModel(blockModel, context))).then((models) => {
    for (const model of models) root.add(model);
    return { object: root, renderTargets: [] };
  });
}

export async function buildItemPreview(
  manifest: ItemPreviewManifest,
  getTextureBytes: PreviewTextureLoader,
): Promise<PreviewBuildResult> {
  const context: BuildContext = { getTextureBytes, textures: new Map(), partialAlpha: new WeakMap() };
  const root = new THREE.Group();
  const models = activeItemModels(manifest);
  const modelTints = activeItemTintColors(manifest.definition);

  for (const [index, model] of models.entries()) {
    const tints = modelTints[index] ?? [];
    if (model.generatedItem) {
      const layers = index === 0 && manifest.layers.length > 0
        ? manifest.layers
        : generatedItemLayers(model);
      if (layers.length > 0) {
        await addGeneratedItemLayers(root, layers, context, tints);
        continue;
      }
    }
    root.add(await buildResolvedModel(model, context, undefined, tints));
  }

  applyDisplayTransform(root, models[0] ?? manifest.model);
  return { object: root, renderTargets: [] };
}

async function addGeneratedItemLayers(
  root: THREE.Group,
  itemLayers: ItemLayer[],
  context: BuildContext,
  tintColors: Array<number | null>,
) {
  const layers = [...itemLayers].sort((a, b) => a.index - b.index);
  const totalDepth = layers.reduce((sum, layer) => sum + Math.max(layer.thickness / 16, 0.001), 0);
  let depthOffset = -totalDepth / 2;

  for (const layer of layers) {
    const resource = await getTextureResource(context, layer.texture);
    const depth = Math.max(layer.thickness / 16, 0.001);
    const geometry = createPixelLayerGeometry(resource.image, depth);
    const material = await createTextureMaterial(context, resource, tintColors[layer.index] ?? 0xffffff);
    const mesh = new THREE.Mesh(geometry, material);
    mesh.position.z = depthOffset + depth / 2;
    root.add(mesh);
    depthOffset += depth;
  }
}

function generatedItemLayers(model: ResolvedModel): ItemLayer[] {
  const layers: ItemLayer[] = [];
  for (let index = 0; ; index += 1) {
    const texture = model.textures[`layer${index}`];
    if (!texture) break;
    layers.push({ index, texture, thickness: 1 });
  }
  return layers;
}

export async function buildEntityPreview(
  manifest: EntityPreviewManifest,
  getTextureBytes: PreviewTextureLoader,
): Promise<PreviewBuildResult> {
  const context: BuildContext = { getTextureBytes, textures: new Map(), partialAlpha: new WeakMap() };
  const texture = await getTextureResource(context, manifest.texture ?? null);
  const textureSize: [number, number] = [
    positiveDimension(manifest.geometry.textureWidth, texture.image.width),
    positiveDimension(manifest.geometry.textureHeight, texture.image.height),
  ];
  const root = new THREE.Group();
  const bones = new Map<string, { group: THREE.Group; pivot: Tuple3 }>();

  for (const bone of manifest.geometry.bones) {
    const pivot = bone.pivot ?? [0, 0, 0];
    const group = new THREE.Group();
    group.name = bone.name;
    const rotation = (bone.rotation ?? [0, 0, 0]).map(THREE.MathUtils.degToRad);
    group.rotation.set(rotation[0], rotation[1], rotation[2]);
    bones.set(bone.name, { group, pivot });
  }

  for (const bone of manifest.geometry.bones) {
    const node = bones.get(bone.name)!;
    const parent = bone.parent ? bones.get(bone.parent) : undefined;
    node.group.position.copy(entityPoint(parent ? subtractPoint(node.pivot, parent.pivot) : node.pivot));
    (parent?.group ?? root).add(node.group);
    for (const cube of bone.cubes) {
      await addEntityCube(node.group, cube, node.pivot, texture, textureSize, context);
    }
  }

  return { object: root, renderTargets: [] };
}

async function addEntityCube(
  bone: THREE.Group,
  cube: EntityCube,
  bonePivot: Tuple3,
  texture: PreviewTextureResource,
  textureSize: [number, number],
  context: BuildContext,
) {
  const origin = cube.origin.map((value) => value - cube.inflate) as Tuple3;
  const size = cube.size.map((value) => value + cube.inflate * 2) as Tuple3;
  const cubePivot = cube.pivot ?? bonePivot;
  const frame = new THREE.Group();
  frame.position.copy(entityPoint(subtractPoint(cubePivot, bonePivot)));
  const rotation = (cube.rotation ?? [0, 0, 0]).map(THREE.MathUtils.degToRad);
  frame.rotation.set(rotation[0], rotation[1], rotation[2]);
  const localOrigin = subtractPoint(origin, cubePivot);
  const faceUvs = entityCubeFaceUvs(cube.uv, cube.size);
  const material = await createTextureMaterial(context, texture, 0xffffff);

  for (const face of FACE_NAMES) {
    const faceUv = faceUvs[face];
    if (!faceUv) continue;
    const [u, v] = faceUv.uv;
    const [width, height] = faceUv.uvSize;
    const faceGeometry = createEntityFaceGeometry(
      localOrigin,
      size,
      face,
      [u, v, u + width, v + height],
      textureSize,
      cube.mirror,
    );
    const mesh = new THREE.Mesh(faceGeometry, material);
    frame.add(mesh);
  }
  bone.add(frame);
}

function entityCubeFaceUvs(
  uv: EntityCubeUv | undefined,
  size: Tuple3,
): Partial<Record<FaceName, EntityFaceUv>> {
  if (!uv) return {};
  if (uv.kind === "perFace") return uv.value;

  const [u, v] = uv.value;
  const [width, height, depth] = size;
  const rect = (x: number, y: number, w: number, h: number): EntityFaceUv => ({
    uv: [x, y],
    uvSize: [w, h],
  });
  return {
    west: rect(u, v + depth, depth, height),
    north: rect(u + depth, v + depth, width, height),
    east: rect(u + depth + width, v + depth, depth, height),
    south: rect(u + depth * 2 + width, v + depth, width, height),
    up: rect(u + depth, v, width, depth),
    down: rect(u + depth + width, v, width, depth),
  };
}

function entityPoint(point: Tuple3) {
  return new THREE.Vector3(point[0] / 16, point[1] / 16, point[2] / 16);
}

function subtractPoint(left: Tuple3, right: Tuple3): Tuple3 {
  return [left[0] - right[0], left[1] - right[1], left[2] - right[2]];
}

function positiveDimension(value: number | null, fallback: number) {
  return value && value > 0 ? value : fallback;
}

function buildBlockModel(blockModel: ResolvedBlockModel, context: BuildContext): Promise<THREE.Group> {
  const stateRotation = createBlockstateRotationMatrix(blockModel.xRotation, blockModel.yRotation);
  return buildResolvedModel(
    blockModel.model,
    context,
    blockModel.uvlock ? stateRotation : undefined,
  ).then((model) => {
    const stateRoot = new THREE.Group();
    stateRoot.add(model);
    stateRoot.matrixAutoUpdate = false;
    stateRoot.matrix.copy(stateRotation);
    return stateRoot;
  });
}

async function buildResolvedModel(
  model: ResolvedModel,
  context: BuildContext,
  uvLockRotation?: THREE.Matrix4,
  tintColors?: Array<number | null>,
): Promise<THREE.Group> {
  const root = new THREE.Group();
  for (const element of model.elements) {
    root.add(await buildElement(element, context, uvLockRotation, tintColors));
  }
  return root;
}

async function buildElement(
  element: ResolvedElement,
  context: BuildContext,
  uvLockRotation?: THREE.Matrix4,
  tintColors?: Array<number | null>,
): Promise<THREE.Group> {
  const elementRoot = new THREE.Group();
  for (const faceName of FACE_NAMES) {
    const face = element.faces[faceName];
    if (!face) continue;
    const resource = await getFaceTexture(context, face);
    const tint = face.tintindex === null
      ? 0xffffff
      : tintColors?.[face.tintindex] ?? tintForFace(face);
    const transparent = await textureHasPartialAlpha(context, resource);
    const material = element.shade
      ? await createTextureMaterial(context, resource, tint, transparent)
      : new THREE.MeshBasicMaterial({ map: resource.texture, color: tint, transparent, alphaTest: 0.01 });
    const geometry = uvLockRotation
      ? createUvLockedBlockFaceGeometry(
        element.from,
        element.to,
        faceName,
        face.uv,
        face.rotation,
        uvLockRotation,
      )
      : createBlockFaceGeometry(element.from, element.to, faceName, face.uv, face.rotation);
    elementRoot.add(new THREE.Mesh(geometry, material));
  }
  if (element.rotation) {
    elementRoot.matrixAutoUpdate = false;
    elementRoot.matrix.copy(createElementRotationMatrix(element.rotation));
  }
  return elementRoot;
}

async function getFaceTexture(context: BuildContext, face: ResolvedFace): Promise<PreviewTextureResource> {
  if (face.missing) return getTextureResource(context, null);
  const path = textureFetchPath(face.texturePath, face.texture);
  return getTextureResource(context, { location: path, path: face.texturePath, missing: false, searchedSources: face.searchedSources });
}

async function getTextureResource(
  context: BuildContext,
  reference: TextureReference | null,
): Promise<PreviewTextureResource> {
  const path = reference?.path ? textureFetchPath(reference.path, reference.location) : reference?.location;
  const cacheKey = reference?.missing || !path ? "__missing__" : path;
  let resource = context.textures.get(cacheKey);
  if (!resource) {
    resource = cacheKey === "__missing__"
      ? Promise.resolve(createMissingTexture())
      : loadPreviewTexture(cacheKey, context.getTextureBytes);
    context.textures.set(cacheKey, resource);
  }
  return resource;
}

async function createTextureMaterial(
  context: BuildContext,
  resource: PreviewTextureResource,
  color: number,
  partialAlpha?: boolean,
): Promise<THREE.MeshLambertMaterial> {
  const transparent = partialAlpha ?? await textureHasPartialAlpha(context, resource);
  return new THREE.MeshLambertMaterial({
    map: resource.texture,
    color,
    transparent,
    alphaTest: 0.01,
  });
}

function textureHasPartialAlpha(context: BuildContext, resource: PreviewTextureResource): Promise<boolean> {
  let result = context.partialAlpha.get(resource.texture);
  if (!result) {
    result = Promise.resolve().then(() => {
      const imageContext = resource.image.getContext("2d", { willReadFrequently: true });
      if (!imageContext) return false;
      try {
        const alpha = imageContext.getImageData(0, 0, resource.image.width, resource.image.height).data;
        for (let index = 3; index < alpha.length; index += 4) {
          if (alpha[index] > PIXEL_ALPHA_THRESHOLD && alpha[index] < 255) return true;
        }
      } catch {
        return false;
      }
      return false;
    });
    context.partialAlpha.set(resource.texture, result);
  }
  return result;
}

function tintForFace(face: ResolvedFace): number {
  const texture = face.texture.toLowerCase();
  if (texture.includes("water")) return DEFAULT_WATER_TINT;
  if (texture.includes("redstone")) return DEFAULT_REDSTONE_TINT;
  if (texture.includes("leaves") || texture.includes("foliage")) return DEFAULT_FOLIAGE_TINT;
  return DEFAULT_GRASS_TINT;
}

function applyDisplayTransform(root: THREE.Group, model: ResolvedModel): void {
  // PreviewはGUI表示を優先し、未定義ならhandheld等の右手持ちtransformを使う。
  const transform = model.display.gui ?? model.display.firstperson_righthand ?? model.display.thirdperson_righthand;
  if (!transform) return;

  const [rx, ry, rz] = transform.rotation.map(THREE.MathUtils.degToRad);
  root.rotation.set(-rx, ry, -rz, "XYZ");
  const translation = minecraftPointToThree([
    8 + transform.translation[0],
    8 + transform.translation[1],
    8 + transform.translation[2],
  ]);
  root.position.copy(translation);
  root.scale.set(...transform.scale);
}

function createPixelLayerGeometry(image: HTMLCanvasElement, depth: number): THREE.BufferGeometry {
  const context = image.getContext("2d", { willReadFrequently: true });
  if (!context) throw new Error("Texture pixel data is unavailable");
  const { width, height } = image;
  const pixels = context.getImageData(0, 0, width, height).data;
  const positions: number[] = [];
  const normals: number[] = [];
  const uvs: number[] = [];
  const indices: number[] = [];
  const halfDepth = depth / 2;
  // Minecraftの生成アイテムは1pxを同じ長さで扱う。軸ごとに割ると長方形テクスチャが正方形化する。
  const pixelSize = 1 / Math.max(width, height);
  const spriteWidth = width * pixelSize;
  const spriteHeight = height * pixelSize;

  const isOpaque = (x: number, y: number) =>
    x >= 0 && y >= 0 && x < width && y < height && pixels[(y * width + x) * 4 + 3] >= PIXEL_ALPHA_THRESHOLD;

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      if (!isOpaque(x, y)) continue;
      const x0 = -spriteWidth / 2 + x * pixelSize;
      const x1 = x0 + pixelSize;
      const y1 = spriteHeight / 2 - y * pixelSize;
      const y0 = y1 - pixelSize;
      const u0 = x / width;
      const u1 = (x + 1) / width;
      const vTop = 1 - y / height;
      const vBottom = 1 - (y + 1) / height;
      const sampleU = (u0 + u1) / 2;
      const sampleV = (vTop + vBottom) / 2;

      appendQuad(positions, normals, uvs, indices,
        [[x0, y1, halfDepth], [x0, y0, halfDepth], [x1, y0, halfDepth], [x1, y1, halfDepth]],
        [0, 0, 1], [[u0, vTop], [u0, vBottom], [u1, vBottom], [u1, vTop]]);
      appendQuad(positions, normals, uvs, indices,
        [[x0, y1, -halfDepth], [x1, y1, -halfDepth], [x1, y0, -halfDepth], [x0, y0, -halfDepth]],
        [0, 0, -1], [[u0, vTop], [u1, vTop], [u1, vBottom], [u0, vBottom]]);

      if (!isOpaque(x - 1, y)) appendQuad(positions, normals, uvs, indices,
        [[x0, y1, halfDepth], [x0, y1, -halfDepth], [x0, y0, -halfDepth], [x0, y0, halfDepth]],
        [-1, 0, 0], [[sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV]]);
      if (!isOpaque(x + 1, y)) appendQuad(positions, normals, uvs, indices,
        [[x1, y1, -halfDepth], [x1, y1, halfDepth], [x1, y0, halfDepth], [x1, y0, -halfDepth]],
        [1, 0, 0], [[sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV]]);
      if (!isOpaque(x, y - 1)) appendQuad(positions, normals, uvs, indices,
        [[x0, y1, -halfDepth], [x0, y1, halfDepth], [x1, y1, halfDepth], [x1, y1, -halfDepth]],
        [0, 1, 0], [[sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV]]);
      if (!isOpaque(x, y + 1)) appendQuad(positions, normals, uvs, indices,
        [[x0, y0, -halfDepth], [x1, y0, -halfDepth], [x1, y0, halfDepth], [x0, y0, halfDepth]],
        [0, -1, 0], [[sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV], [sampleU, sampleV]]);
    }
  }

  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute("position", new THREE.Float32BufferAttribute(positions, 3));
  geometry.setAttribute("normal", new THREE.Float32BufferAttribute(normals, 3));
  geometry.setAttribute("uv", new THREE.Float32BufferAttribute(uvs, 2));
  geometry.setIndex(indices);
  return geometry;
}

function appendQuad(
  positions: number[],
  normals: number[],
  uvs: number[],
  indices: number[],
  points: number[][],
  normal: number[],
  faceUvs: number[][],
): void {
  const start = positions.length / 3;
  for (let index = 0; index < 4; index += 1) {
    positions.push(...points[index]);
    normals.push(...normal);
    uvs.push(...faceUvs[index]);
  }
  indices.push(start, start + 1, start + 2, start, start + 2, start + 3);
}
