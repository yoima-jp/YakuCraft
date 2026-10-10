import * as THREE from "three";
import type { ElementRotation, FaceName, Tuple3, Tuple4 } from "../../types/preview";

const PIXELS_PER_BLOCK = 16;

/** Minecraftの原点は左上・Y下向きなので、中央原点のThree.js座標へ移す。 */
export function minecraftPointToThree(point: Tuple3): THREE.Vector3 {
  return new THREE.Vector3(
    (point[0] - PIXELS_PER_BLOCK / 2) / PIXELS_PER_BLOCK,
    (PIXELS_PER_BLOCK / 2 - point[1]) / PIXELS_PER_BLOCK,
    (point[2] - PIXELS_PER_BLOCK / 2) / PIXELS_PER_BLOCK,
  );
}

/** Minecraftの左上原点UVをThree.jsの下原点UVへ変換し、面の90度回転も適用する。 */
export function minecraftUvCornersToThree(
  uv: Tuple4,
  rotation: number,
): number[] {
  const [u1, v1, u2, v2] = uv;
  const corners = [
    [u1 / 16, 1 - v1 / 16],
    [u2 / 16, 1 - v1 / 16],
    [u2 / 16, 1 - v2 / 16],
    [u1 / 16, 1 - v2 / 16],
  ];
  const steps = ((Math.round(rotation / 90) % 4) + 4) % 4;

  return Array.from({ length: 4 }, (_, index) => corners[(index - steps + 4) % 4]).flat();
}

/** Elementの回転をMinecraftのpx座標とThree.jsのY軸反転を考慮した行列にする。 */
export function createElementRotationMatrix(rotation: ElementRotation): THREE.Matrix4 {
  const pivot = minecraftPointToThree(rotation.origin);
  const radians = THREE.MathUtils.degToRad(rotation.angle);
  // Y軸を反転する座標変換では、X/Z軸の正回転方向だけがThree.jsと逆になる。
  const signedAngle = rotation.axis === "y" ? radians : -radians;
  const axis = new THREE.Vector3(
    rotation.axis === "x" ? 1 : 0,
    rotation.axis === "y" ? 1 : 0,
    rotation.axis === "z" ? 1 : 0,
  );
  const factor = rotation.rescale
    ? 1 / Math.max(Math.abs(Math.cos(radians)), 0.01)
    : 1;
  const scale = new THREE.Vector3(
    rotation.axis === "x" ? 1 : factor,
    rotation.axis === "y" ? 1 : factor,
    rotation.axis === "z" ? 1 : factor,
  );

  return new THREE.Matrix4()
    .makeTranslation(pivot.x, pivot.y, pivot.z)
    .multiply(new THREE.Matrix4().makeRotationAxis(axis, signedAngle))
    .multiply(new THREE.Matrix4().makeScale(scale.x, scale.y, scale.z))
    .multiply(new THREE.Matrix4().makeTranslation(-pivot.x, -pivot.y, -pivot.z));
}

/** Blockstateのx/y回転をブロック中心の周りに適用する。 */
export function createBlockstateRotationMatrix(xDegrees: number, yDegrees: number): THREE.Matrix4 {
  const xRotation = new THREE.Matrix4().makeRotationX(-THREE.MathUtils.degToRad(xDegrees));
  const yRotation = new THREE.Matrix4().makeRotationY(THREE.MathUtils.degToRad(yDegrees));
  // Minecraftのx回転を先に行い、その結果をy回転する順序にする。
  return yRotation.multiply(xRotation);
}

const FACE_NORMALS: Record<FaceName, THREE.Vector3> = {
  down: new THREE.Vector3(0, -1, 0),
  up: new THREE.Vector3(0, 1, 0),
  north: new THREE.Vector3(0, 0, -1),
  south: new THREE.Vector3(0, 0, 1),
  west: new THREE.Vector3(-1, 0, 0),
  east: new THREE.Vector3(1, 0, 0),
};

const FACE_UV_AXES: Record<FaceName, { u: THREE.Vector3; v: THREE.Vector3 }> = {
  down: { u: new THREE.Vector3(1, 0, 0), v: new THREE.Vector3(0, 0, 1) },
  up: { u: new THREE.Vector3(1, 0, 0), v: new THREE.Vector3(0, 0, 1) },
  north: { u: new THREE.Vector3(-1, 0, 0), v: new THREE.Vector3(0, -1, 0) },
  south: { u: new THREE.Vector3(1, 0, 0), v: new THREE.Vector3(0, -1, 0) },
  west: { u: new THREE.Vector3(0, 0, 1), v: new THREE.Vector3(0, -1, 0) },
  east: { u: new THREE.Vector3(0, 0, -1), v: new THREE.Vector3(0, -1, 0) },
};

// yc-assets::model::default_uv の矩形corner（左上=0、右上=1、右下=2、左下=3）を、このファイルの頂点順へ対応させる。
// 面 / yc-assetsのdefault UV / pointsの頂点が参照するcorner:
// down  [from.x,16-to.z,to.x,16-from.z]       from.x,to.y,from.z→3; to.x,to.y,from.z→2; to.x,to.y,to.z→1; from.x,to.y,to.z→0
// up    [from.x,from.z,to.x,to.z]             from.x,from.y,from.z→0; from.x,from.y,to.z→3; to.x,from.y,to.z→2; to.x,from.y,from.z→1
// north [16-to.x,16-to.y,16-from.x,16-from.y] from.x,from.y,from.z→2; to.x,from.y,from.z→3; to.x,to.y,from.z→0; from.x,to.y,from.z→1
// south [from.x,16-to.y,to.x,16-from.y]        from.x,from.y,to.z→3; from.x,to.y,to.z→0; to.x,to.y,to.z→1; to.x,from.y,to.z→2
// west  [from.z,16-to.y,to.z,16-from.y]        from.x,from.y,from.z→3; from.x,to.y,from.z→0; from.x,to.y,to.z→1; from.x,from.y,to.z→2
// east  [16-to.z,16-to.y,16-from.z,16-from.y] to.x,from.y,from.z→2; to.x,from.y,to.z→3; to.x,to.y,to.z→0; to.x,to.y,from.z→1
const FACE_UV_ORDER: Record<FaceName, [number, number, number, number]> = {
  down: [3, 2, 1, 0],
  up: [0, 3, 2, 1],
  north: [2, 3, 0, 1],
  south: [3, 0, 1, 2],
  west: [3, 0, 1, 2],
  east: [2, 3, 0, 1],
};

// FACE_UV_AXES上の左上・右上・右下・左下を、各面のUV corner番号へ対応させる。
// 面ごとに頂点順が異なるため、軸の符号だけで共通corner番号を使うと
// Blockstate回転がidentityでもsouth/north/down等のUV順が崩れる。
const FACE_AXIS_UV_CORNERS: Record<FaceName, [number, number, number, number]> = {
  down: [3, 2, 1, 0],
  up: [0, 1, 2, 3],
  north: [3, 2, 1, 0],
  south: [3, 2, 1, 0],
  west: [3, 2, 1, 0],
  east: [3, 2, 1, 0],
};

/** 一つのBlock faceを、各面の法線とUVを持つquad geometryとして作る。 */
export function createBlockFaceGeometry(
  from: Tuple3,
  to: Tuple3,
  face: FaceName,
  uv: Tuple4,
  uvRotation: number,
): THREE.BufferGeometry {
  const x1 = from[0];
  const x2 = to[0];
  const yTop = from[1];
  const yBottom = to[1];
  const z1 = from[2];
  const z2 = to[2];
  const points: Record<FaceName, Tuple3[]> = {
    up: [[x1, yTop, z1], [x1, yTop, z2], [x2, yTop, z2], [x2, yTop, z1]],
    down: [[x1, yBottom, z1], [x2, yBottom, z1], [x2, yBottom, z2], [x1, yBottom, z2]],
    north: [[x1, yTop, z1], [x2, yTop, z1], [x2, yBottom, z1], [x1, yBottom, z1]],
    south: [[x1, yTop, z2], [x1, yBottom, z2], [x2, yBottom, z2], [x2, yTop, z2]],
    west: [[x1, yTop, z1], [x1, yBottom, z1], [x1, yBottom, z2], [x1, yTop, z2]],
    east: [[x2, yTop, z1], [x2, yTop, z2], [x2, yBottom, z2], [x2, yBottom, z1]],
  };
  const positions = points[face].flatMap((point) => minecraftPointToThree(point).toArray());
  const normal = FACE_NORMALS[face].toArray();
  const normals = [...normal, ...normal, ...normal, ...normal];
  const geometry = new THREE.BufferGeometry();

  geometry.setAttribute("position", new THREE.Float32BufferAttribute(positions, 3));
  geometry.setAttribute("normal", new THREE.Float32BufferAttribute(normals, 3));
  const textureCorners = minecraftUvCornersToThree(uv, uvRotation);
  const faceUvs = FACE_UV_ORDER[face].flatMap((corner) => textureCorners.slice(corner * 2, corner * 2 + 2));
  geometry.setAttribute("uv", new THREE.Float32BufferAttribute(faceUvs, 2));
  geometry.setIndex([0, 1, 2, 0, 2, 3]);
  return geometry;
}

/** Blockstate回転後も同じworld面へtexture cornerを固定するuvlock用geometry。 */
export function createUvLockedBlockFaceGeometry(
  from: Tuple3,
  to: Tuple3,
  face: FaceName,
  uv: Tuple4,
  uvRotation: number,
  blockRotation: THREE.Matrix4,
): THREE.BufferGeometry {
  const geometry = createBlockFaceGeometry(from, to, face, uv, uvRotation);
  const positions = geometry.getAttribute("position");
  const transformedNormal = FACE_NORMALS[face].clone().transformDirection(blockRotation);
  const worldFace = (Object.entries(FACE_NORMALS) as Array<[FaceName, THREE.Vector3]>)
    .find(([, normal]) => normal.dot(transformedNormal) > 0.999)?.[0];
  if (!worldFace) return geometry;

  const axes = FACE_UV_AXES[worldFace];
  const transformedPoints = Array.from({ length: positions.count }, (_, index) =>
    new THREE.Vector3(positions.getX(index), positions.getY(index), positions.getZ(index))
      .applyMatrix4(blockRotation),
  );
  const uValues = transformedPoints.map((point) => point.dot(axes.u));
  const vValues = transformedPoints.map((point) => point.dot(axes.v));
  const uMiddle = (Math.min(...uValues) + Math.max(...uValues)) / 2;
  const vMiddle = (Math.min(...vValues) + Math.max(...vValues)) / 2;
  const textureCorners = minecraftUvCornersToThree(uv, uvRotation);
  const lockedUvs = transformedPoints.flatMap((point) => {
    const right = point.dot(axes.u) > uMiddle;
    const bottom = point.dot(axes.v) > vMiddle;
    const axisCorner = bottom ? (right ? 2 : 3) : (right ? 1 : 0);
    const corner = FACE_AXIS_UV_CORNERS[worldFace][axisCorner];
    return textureCorners.slice(corner * 2, corner * 2 + 2);
  });
  geometry.setAttribute("uv", new THREE.Float32BufferAttribute(lockedUvs, 2));
  return geometry;
}

/** GeckoLib/Bedrock cubeの一面をentity atlasの実寸UVで作る。Entity座標はY上向き。 */
export function createEntityFaceGeometry(
  origin: Tuple3,
  size: Tuple3,
  face: FaceName,
  uv: Tuple4,
  textureSize: [number, number],
  mirror: boolean,
): THREE.BufferGeometry {
  const [x1, y1, z1] = origin;
  const [x2, y2, z2] = [x1 + size[0], y1 + size[1], z1 + size[2]];
  const points: Record<FaceName, Tuple3[]> = {
    up: [[x1, y2, z1], [x1, y2, z2], [x2, y2, z2], [x2, y2, z1]],
    down: [[x1, y1, z1], [x2, y1, z1], [x2, y1, z2], [x1, y1, z2]],
    north: [[x1, y2, z1], [x2, y2, z1], [x2, y1, z1], [x1, y1, z1]],
    south: [[x1, y2, z2], [x1, y1, z2], [x2, y1, z2], [x2, y2, z2]],
    west: [[x1, y2, z1], [x1, y1, z1], [x1, y1, z2], [x1, y2, z2]],
    east: [[x2, y2, z1], [x2, y2, z2], [x2, y1, z2], [x2, y1, z1]],
  };
  const positions = points[face].flatMap(([x, y, z]) => [x / 16, y / 16, z / 16]);
  const normal = FACE_NORMALS[face].toArray();
  const [u1, v1, u2, v2] = uv;
  const left = mirror ? u2 : u1;
  const right = mirror ? u1 : u2;
  const [textureWidth, textureHeight] = textureSize;
  const corners = [
    [left / textureWidth, 1 - v1 / textureHeight],
    [right / textureWidth, 1 - v1 / textureHeight],
    [right / textureWidth, 1 - v2 / textureHeight],
    [left / textureWidth, 1 - v2 / textureHeight],
  ];
  const faceUvs = FACE_UV_ORDER[face].flatMap((corner) => corners[corner]);
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute("position", new THREE.Float32BufferAttribute(positions, 3));
  geometry.setAttribute("normal", new THREE.Float32BufferAttribute([...normal, ...normal, ...normal, ...normal], 3));
  geometry.setAttribute("uv", new THREE.Float32BufferAttribute(faceUvs, 2));
  geometry.setIndex([0, 1, 2, 0, 2, 3]);
  return geometry;
}

/** Scene差し替え時にGPU資源を重複なく解放する。RenderTargetのtextureも明示的に回収する。 */
export function disposeObjectTree(
  root: THREE.Object3D,
  renderTargets: THREE.WebGLRenderTarget[] = [],
): void {
  const geometries = new Set<THREE.BufferGeometry>();
  const materials = new Set<THREE.Material>();
  const textures = new Set<THREE.Texture>();
  const targets = new Set(renderTargets);

  root.traverse((object) => {
    const renderable = object as THREE.Object3D & {
      geometry?: THREE.BufferGeometry;
      material?: THREE.Material | THREE.Material[];
    };
    if (renderable.geometry) geometries.add(renderable.geometry);
    if (!renderable.material) return;

    for (const material of Array.isArray(renderable.material) ? renderable.material : [renderable.material]) {
      materials.add(material);
      for (const value of Object.values(material)) {
        if (value && typeof value === "object" && "isTexture" in value && value.isTexture) {
          textures.add(value as THREE.Texture);
        }
      }
    }
  });

  for (const target of targets) textures.add(target.texture);
  for (const geometry of geometries) geometry.dispose();
  for (const material of materials) material.dispose();
  for (const texture of textures) texture.dispose();
  for (const target of targets) target.dispose();
}
