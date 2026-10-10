import type * as THREE from "three";

/** CSS pixel寸法を描画バッファと透視投影へ同時に適用し、canvasの引き伸ばしを防ぐ。 */
export function resizePreviewRenderer(
  renderer: Pick<THREE.WebGLRenderer, "setPixelRatio" | "setSize">,
  camera: THREE.PerspectiveCamera,
  width: number,
  height: number,
  devicePixelRatio: number,
): void {
  const cssWidth = Math.max(width, 1);
  const cssHeight = Math.max(height, 1);
  renderer.setPixelRatio(Math.min(devicePixelRatio || 1, 2));
  renderer.setSize(cssWidth, cssHeight, false);
  camera.aspect = cssWidth / cssHeight;
  camera.updateProjectionMatrix();
}
