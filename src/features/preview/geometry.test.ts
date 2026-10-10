import * as THREE from "three";
import { describe, expect, it, vi } from "vitest";
import { createBlockFaceGeometry, createBlockstateRotationMatrix, createElementRotationMatrix, createUvLockedBlockFaceGeometry, disposeObjectTree, minecraftUvCornersToThree } from "./geometry";
import { previewKindOrNoPreview } from "../../types/preview";

describe("preview geometry helpers", () => {
  it("converts Minecraft top-left UV coordinates and face rotation", () => {
    expect(minecraftUvCornersToThree([0, 0, 16, 16], 0)).toEqual([
      0, 1, 1, 1, 1, 0, 0, 0,
    ]);
    expect(minecraftUvCornersToThree([0, 0, 16, 16], 90)).toEqual([
      0, 0, 0, 1, 1, 1, 1, 0,
    ]);
  });

  it("maps each face's asymmetric texture corners to the yc-assets default UV vertices", () => {
    const uv: [number, number, number, number] = [2, 3, 11, 13];
    const textureCorners = minecraftUvCornersToThree(uv, 0);
    const cornerColors = ["red", "green", "blue", "yellow"] as const;
    const expected: Record<string, number[]> = {
      down: [3, 2, 1, 0],
      up: [0, 3, 2, 1],
      north: [2, 3, 0, 1],
      south: [3, 0, 1, 2],
      west: [3, 0, 1, 2],
      east: [2, 3, 0, 1],
    };

    for (const [face, cornerOrder] of Object.entries(expected)) {
      const geometry = createBlockFaceGeometry(
        [2, 4, 1],
        [13, 12, 9],
        face as "down" | "up" | "north" | "south" | "west" | "east",
        [...uv],
        0,
      );
      const actualUvs = Array.from(geometry.getAttribute("uv").array);
      const expectedUvs = cornerOrder.flatMap((corner) =>
        textureCorners.slice(corner * 2, corner * 2 + 2),
      );
      const sampledColors = Array.from({ length: 4 }, (_, vertex) => {
        const sample = actualUvs.slice(vertex * 2, vertex * 2 + 2);
        const corner = Array.from({ length: 4 }, (_, index) => index).findIndex((index) =>
          textureCorners[index * 2] === sample[0] && textureCorners[index * 2 + 1] === sample[1],
        );
        return cornerColors[corner];
      });

      expect(actualUvs, `${face} face`).toEqual(expectedUvs);
      expect(sampledColors, `${face} asymmetric texture direction`).toEqual(
        cornerOrder.map((corner) => cornerColors[corner]),
      );
      geometry.dispose();
    }
  });

  it("keeps an element pivot fixed and converts the Minecraft axis direction", () => {
    const matrix = createElementRotationMatrix({
      origin: [8, 8, 8],
      axis: "y",
      angle: 90,
      rescale: false,
    });
    const pivot = new THREE.Vector3(0, 0, 0).applyMatrix4(matrix);
    const point = new THREE.Vector3(1, 0, 0).applyMatrix4(matrix);

    expect(pivot.toArray()).toEqual([0, 0, 0]);
    expect(point.x).toBeCloseTo(0);
    expect(point.z).toBeCloseTo(-1);
  });

  it("applies blockstate X then Y rotation around the model center", () => {
    const xRotated = new THREE.Vector3(0, 1, 0).applyMatrix4(createBlockstateRotationMatrix(90, 0));
    const yRotated = new THREE.Vector3(1, 0, 0).applyMatrix4(createBlockstateRotationMatrix(0, 90));

    expect(xRotated.z).toBeCloseTo(-1);
    expect(yRotated.z).toBeCloseTo(-1);
  });

  it("locks top-face UV corners to world axes after blockstate rotation", () => {
    const from: [number, number, number] = [1, 2, 3];
    const to: [number, number, number] = [13, 14, 15];
    const uv: [number, number, number, number] = [2, 3, 11, 13];
    const identity = createBlockstateRotationMatrix(0, 0);
    const turn = createBlockstateRotationMatrix(0, 90);
    const ordinary = createBlockFaceGeometry(from, to, "up", uv, 90);
    const lockedIdentity = createUvLockedBlockFaceGeometry(from, to, "up", uv, 90, identity);
    const lockedTurn = createUvLockedBlockFaceGeometry(from, to, "up", uv, 90, turn);

    expect(Array.from(lockedIdentity.getAttribute("uv").array)).toEqual(
      Array.from(ordinary.getAttribute("uv").array),
    );
    expect(Array.from(lockedTurn.getAttribute("uv").array)).not.toEqual(
      Array.from(ordinary.getAttribute("uv").array),
    );
    ordinary.dispose();
    lockedIdentity.dispose();
    lockedTurn.dispose();
  });

  it("disposes unique geometry, material, texture, and render target resources", () => {
    const geometry = new THREE.BufferGeometry();
    const texture = new THREE.Texture();
    const material = new THREE.MeshBasicMaterial({ map: texture });
    const target = new THREE.WebGLRenderTarget(4, 4);
    const disposeGeometry = vi.spyOn(geometry, "dispose");
    const disposeTexture = vi.spyOn(texture, "dispose");
    const disposeMaterial = vi.spyOn(material, "dispose");
    const disposeTarget = vi.spyOn(target, "dispose");
    const root = new THREE.Group();
    root.add(new THREE.Mesh(geometry, material));

    disposeObjectTree(root, [target]);

    expect(disposeGeometry).toHaveBeenCalledOnce();
    expect(disposeMaterial).toHaveBeenCalledOnce();
    expect(disposeTexture).toHaveBeenCalledOnce();
    expect(disposeTarget).toHaveBeenCalledOnce();
  });

  it("maps an unrecognized IR variant to No Preview", () => {
    expect(previewKindOrNoPreview({ kind: "futurePreview", manifest: { geometry: {} } })).toBe("noPreview");
  });
});
