import * as THREE from "three";
import type { PreviewTextureLoader } from "../../types/preview";

export interface PreviewTextureResource {
  texture: THREE.CanvasTexture;
  image: HTMLCanvasElement;
  missing: boolean;
}

interface AnimationMetadata {
  animation?: {
    width?: number;
    height?: number;
    frames?: Array<number | { index?: number }>;
  };
}

/** Uint8ArrayをPNGとして読み、mcmetaがある縦長画像は最初のanimation frameに切り出す。 */
export async function loadPreviewTexture(
  path: string,
  getTextureBytes: PreviewTextureLoader,
): Promise<PreviewTextureResource> {
  try {
    const [bytes, metadata] = await Promise.all([
      getTextureBytes(path),
      getTextureBytes(`${path}.mcmeta`).then(parseAnimationMetadata).catch(() => null),
    ]);
    const image = await decodePng(bytes);
    const canvas = createFirstFrameCanvas(image, metadata);
    return { texture: createCanvasTexture(canvas), image: canvas, missing: false };
  } catch {
    return createMissingTexture();
  }
}

/** 取得失敗時は参照先を隠さないcheckerを返し、3D sceneの描画を続ける。 */
export function createMissingTexture(): PreviewTextureResource {
  const canvas = document.createElement("canvas");
  canvas.width = 16;
  canvas.height = 16;
  const context = canvas.getContext("2d");
  if (context) {
    const theme = getComputedStyle(document.documentElement);
    context.fillStyle = theme.getPropertyValue("--color-preview-missing-primary").trim();
    context.fillRect(0, 0, 16, 16);
    context.fillStyle = theme.getPropertyValue("--color-preview-missing-secondary").trim();
    for (let y = 0; y < 16; y += 8) {
      for (let x = (y / 8) % 2 === 0 ? 0 : 8; x < 16; x += 16) {
        context.fillRect(x, y, 8, 8);
      }
    }
  }
  return { texture: createCanvasTexture(canvas), image: canvas, missing: true };
}

function createCanvasTexture(canvas: HTMLCanvasElement): THREE.CanvasTexture {
  const texture = new THREE.CanvasTexture(canvas);
  texture.magFilter = THREE.NearestFilter;
  texture.minFilter = THREE.NearestFilter;
  texture.generateMipmaps = false;
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.needsUpdate = true;
  return texture;
}

function parseAnimationMetadata(bytes: Uint8Array): AnimationMetadata | null {
  const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes));
  if (!parsed || typeof parsed !== "object" || !("animation" in parsed)) return null;
  const animation = (parsed as { animation: unknown }).animation;
  return animation && typeof animation === "object" ? parsed as AnimationMetadata : null;
}

function decodePng(bytes: Uint8Array): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const data = new Uint8Array(bytes).buffer;
    // Base64化でbinaryを膨らませず、decode完了後は参照を残さないようObject URLをすぐ解放する。
    const objectUrl = URL.createObjectURL(new Blob([data], { type: "image/png" }));
    const image = new Image();
    image.onload = () => {
      URL.revokeObjectURL(objectUrl);
      resolve(image);
    };
    image.onerror = () => {
      URL.revokeObjectURL(objectUrl);
      reject(new Error("Texture image could not be decoded"));
    };
    image.src = objectUrl;
  });
}

function createFirstFrameCanvas(
  image: HTMLImageElement,
  metadata: AnimationMetadata | null,
): HTMLCanvasElement {
  const animation = metadata?.animation;
  const isVerticalSheet = image.naturalHeight > image.naturalWidth;
  const frameWidth = Math.min(positiveInteger(animation?.width, image.naturalWidth), image.naturalWidth);
  const frameHeight = Math.min(positiveInteger(animation?.height, frameWidth), image.naturalHeight);
  const frame = animation?.frames?.[0];
  const requestedFrame = typeof frame === "number" ? frame : frame?.index ?? 0;
  const columns = Math.max(1, Math.floor(image.naturalWidth / frameWidth));
  const rows = Math.max(1, Math.floor(image.naturalHeight / frameHeight));
  const frameIndex = Number.isFinite(requestedFrame)
    ? Math.min(Math.max(Math.floor(requestedFrame), 0), columns * rows - 1)
    : 0;
  const frameX = (frameIndex % columns) * frameWidth;
  const frameY = Math.floor(frameIndex / columns) * frameHeight;
  const shouldCrop = Boolean(metadata && isVerticalSheet && frameHeight > 0);
  const canvas = document.createElement("canvas");
  canvas.width = shouldCrop ? frameWidth : image.naturalWidth;
  canvas.height = shouldCrop ? frameHeight : image.naturalHeight;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("Canvas is unavailable");
  context.imageSmoothingEnabled = false;
  if (shouldCrop) {
    context.drawImage(image, frameX, frameY, frameWidth, frameHeight, 0, 0, frameWidth, frameHeight);
  } else {
    context.drawImage(image, 0, 0);
  }
  return canvas;
}

function positiveInteger(value: number | undefined, fallback: number): number {
  return value !== undefined && Number.isFinite(value) && value > 0 ? Math.max(1, Math.floor(value)) : fallback;
}
