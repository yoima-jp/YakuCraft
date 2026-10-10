import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PreviewManifest } from "../../types/preview";
import type * as THREE from "three";
import { Preview } from "./Preview";

interface RendererRecord {
  sizes: Array<[number, number, boolean]>;
  pixelRatios: number[];
  aspects: number[];
  backgrounds: Array<number | null>;
}

const rendererState = vi.hoisted(() => ({ instances: [] as RendererRecord[] }));

vi.mock("three", async (importOriginal) => {
  const actual = await importOriginal<typeof import("three")>();
  class MockWebGLRenderer {
    readonly record: RendererRecord = { sizes: [], pixelRatios: [], aspects: [], backgrounds: [] };
    outputColorSpace = "";

    constructor() {
      rendererState.instances.push(this.record);
    }

    setPixelRatio(pixelRatio: number) {
      this.record.pixelRatios.push(pixelRatio);
    }

    setSize(width: number, height: number, updateStyle = false) {
      this.record.sizes.push([width, height, updateStyle]);
    }

    render(scene: THREE.Scene, camera: THREE.Camera) {
      const perspectiveCamera = camera as THREE.PerspectiveCamera;
      this.record.aspects.push(perspectiveCamera.aspect);
      this.record.backgrounds.push(scene.background instanceof actual.Color ? scene.background.getHex() : null);
    }

    dispose() {}
  }

  return {
    ...actual,
    WebGLRenderer: MockWebGLRenderer as unknown as typeof actual.WebGLRenderer,
  };
});

vi.mock("./sceneBuilder", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./sceneBuilder")>();
  return {
    ...actual,
    buildBlockPreview: async () => {
      const THREE = await import("three");
      const object = new THREE.Group();
      object.add(new THREE.Mesh(new THREE.BoxGeometry(1, 1, 1), new THREE.MeshBasicMaterial()));
      return { object, renderTargets: [] };
    },
  };
});

class TestResizeObserver implements ResizeObserver {
  constructor(callback: ResizeObserverCallback) {
    activeResizeObserver = {
      trigger: () => callback([], {} as ResizeObserver),
    };
  }

  observe(_target: Element) {}

  unobserve(_target: Element) {}

  disconnect() {}

}

let activeResizeObserver: { trigger: () => void } | undefined;

const manifest: PreviewManifest = {
  kind: "block",
  manifest: {
    translationKey: "block.example.cube",
    variants: [],
    selectedVariant: null,
    properties: [],
    selectedState: {},
    models: [{
      model: {
        model: "example:block/cube",
        source: null,
        sourceChain: [],
        textures: {},
        elements: [],
        ambientOcclusion: true,
        display: {},
        guiLight: null,
        generatedItem: false,
      },
      xRotation: 0,
      yRotation: 0,
      uvlock: false,
    }],
  },
};

describe("Preview viewport", () => {
  beforeEach(() => {
    rendererState.instances.splice(0);
    activeResizeObserver = undefined;
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
  });

  afterEach(() => {
    cleanup();
    document.documentElement.style.removeProperty("--color-preview-background");
    delete document.documentElement.dataset.theme;
    vi.unstubAllGlobals();
  });

  it("テーマ変更で既存のSceneの背景を更新し、Rendererを作り直さない", async () => {
    document.documentElement.style.setProperty("--color-preview-background", "#fafbf9");
    const view = render(<Preview manifest={manifest} getTextureBytes={vi.fn(async () => new Uint8Array())} getModelJson={vi.fn(async () => null)} />);
    await waitFor(() => expect(rendererState.instances[0]?.backgrounds.at(-1)).toBe(0xfafbf9));
    act(() => {
      document.documentElement.style.setProperty("--color-preview-background", "#626862");
      document.documentElement.dataset.theme = "dark";
    });
    await waitFor(() => expect(rendererState.instances[0]?.backgrounds.at(-1)).toBe(0x626862));
    expect(rendererState.instances).toHaveLength(1);
    view.unmount();
    const renderCount = rendererState.instances[0].backgrounds.length;
    await act(async () => { document.documentElement.dataset.theme = "light"; });
    expect(rendererState.instances[0].backgrounds).toHaveLength(renderCount);
  });

  it("tracks the canvas aspect ratio when its viewport resizes and has no reset button", async () => {
    const { container } = render(
      <Preview
        manifest={manifest}
        getTextureBytes={vi.fn(async () => new Uint8Array())}
        getModelJson={vi.fn(async () => null)}
      />,
    );
    const viewport = container.querySelector(".preview__viewport");
    if (!viewport) throw new Error("Preview viewport was not rendered");
    Object.defineProperties(viewport, {
      clientWidth: { configurable: true, value: 400 },
      clientHeight: { configurable: true, value: 200 },
    });

    await waitFor(() => expect(rendererState.instances).toHaveLength(1));
    await act(async () => {
      activeResizeObserver?.trigger();
    });

    await waitFor(() => {
      const renderer = rendererState.instances[0];
      expect(renderer.sizes.at(-1)).toEqual([400, 200, false]);
      expect(renderer.aspects.at(-1)).toBe(2);
    });
    expect(screen.queryByRole("button", { name: "リセット" })).toBeNull();
  });
});
