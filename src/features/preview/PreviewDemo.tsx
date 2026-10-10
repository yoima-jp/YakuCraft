import { useCallback, useMemo, useState } from "react";
import { Preview } from "./Preview";
import "./preview-demo.css";
import type {
  AssetLocation,
  BlockPreviewManifest,
  ItemPreviewManifest,
  PreviewManifest,
  ResolvedElement,
  ResolvedFace,
  ResolvedModel,
  TextureReference,
} from "../../types/preview";

const CASES = ["cube_all", "stairs", "stairs rotated", "generated item", "missing texture"] as const;
type DemoCase = (typeof CASES)[number];
const CASE_LABELS: Record<DemoCase, string> = {
  cube_all: "全面テクスチャ",
  stairs: "階段",
  "stairs rotated": "回転した階段",
  "generated item": "生成アイテム",
  "missing texture": "テクスチャなし",
};

const GRASS_ASSET = asset("assets/demo/textures/block/cube_all.png", "texture_block");
const ITEM_ASSET = asset("assets/demo/textures/item/generated.png", "texture_item");

export function PreviewDemo() {
  const [selected, setSelected] = useState<DemoCase>(CASES[0]);
  const assets = useMemo(() => createDemoTextureBytes(), []);
  const getTextureBytes = useCallback(async (texturePath: string) => {
    const bytes = (await assets).get(texturePath);
    if (!bytes) throw new Error(`No demo texture for ${texturePath}`);
    return bytes;
  }, [assets]);
  const getModelJson = useCallback(async (source: AssetLocation) => {
    if (source.archivePath.endsWith("/stairs.json")) {
      return { parent: "minecraft:block/stairs", textures: { all: "demo:block/cube_all" }, elements: [{ from: [0, 8, 0], to: [16, 16, 16] }, { from: [0, 0, 0], to: [16, 8, 8] }] };
    }
    if (source.archivePath.endsWith("/cube_all.json")) {
      return { parent: "minecraft:block/cube_all", textures: { all: "demo:block/cube_all" } };
    }
    if (source.archivePath.endsWith("/generated.json")) {
      return { parent: "minecraft:item/generated", textures: { layer0: "demo:item/generated" } };
    }
    if (source.archivePath.endsWith("/missing.json")) {
      return { parent: "minecraft:block/cube_all", textures: { all: "demo:block/missing" } };
    }
    throw new Error(`No demo model for ${source.archivePath}`);
  }, []);

  return (
    <main className="preview-demo">
      <header className="preview-demo__header">
        <h1>プレビューの動作確認</h1>
        <nav aria-label="プレビュー例">
          {CASES.map((name) => (
            <button
              key={name}
              type="button"
              aria-pressed={selected === name}
              onClick={() => setSelected(name)}
            >
              {CASE_LABELS[name]}
            </button>
          ))}
        </nav>
      </header>
      <Preview manifest={createManifest(selected)} getTextureBytes={getTextureBytes} getModelJson={getModelJson} />
    </main>
  );
}

function createManifest(selected: DemoCase): PreviewManifest {
  if (selected === "generated item") return createGeneratedItem();
  if (selected === "missing texture") return createMissingBlock();
  const model = createBlockModel(selected === "stairs" || selected === "stairs rotated");
  const block: BlockPreviewManifest = {
    translationKey: `block.demo.${selected.replaceAll(" ", "_")}`,
    variants: [],
    selectedVariant: null,
    properties: [],
    selectedState: {},
    models: [{
      model,
      xRotation: 0,
      yRotation: selected === "stairs rotated" ? 90 : 0,
      uvlock: false,
    }],
  };
  return { kind: "block", manifest: block };
}

function createBlockModel(stairs: boolean): ResolvedModel {
  const elements: ResolvedElement[] = stairs
    ? [
        element([0, 8, 0], [16, 16, 16], GRASS_ASSET),
        element([0, 0, 0], [16, 8, 8], GRASS_ASSET),
      ]
    : [element([0, 0, 0], [16, 16, 16], GRASS_ASSET)];
  return {
    model: stairs ? "demo:block/stairs" : "demo:block/cube_all",
    source: asset(`assets/demo/models/block/${stairs ? "stairs" : "cube_all"}.json`, "block_model"),
    sourceChain: [asset(`assets/demo/models/block/${stairs ? "stairs" : "cube_all"}.json`, "block_model")],
    textures: { all: textureReference("demo:block/cube_all", GRASS_ASSET) },
    elements,
    ambientOcclusion: true,
    display: {},
    guiLight: "side",
    generatedItem: false,
  };
}

function createGeneratedItem(): PreviewManifest {
  const texture = textureReference("demo:item/generated", ITEM_ASSET);
  const item: ItemPreviewManifest = {
    translationKey: "item.demo.generated",
    model: {
      model: "demo:item/generated",
      source: asset("assets/demo/models/item/generated.json", "item_model"),
      sourceChain: [asset("assets/demo/models/item/generated.json", "item_model")],
      textures: { layer0: texture },
      elements: [],
      ambientOcclusion: false,
      display: {
        gui: { rotation: [30, 225, 0], translation: [0, 0, 0], scale: [0.625, 0.625, 0.625] },
      },
      guiLight: "front",
      generatedItem: true,
    },
    layers: [{ index: 0, texture, thickness: 1 }],
    models: [],
    definition: null,
    variants: [],
  };
  return { kind: "item", manifest: item };
}

function createMissingBlock(): PreviewManifest {
  const missingFace: ResolvedFace = {
    texture: "demo:block/missing",
    texturePath: null,
    uv: [0, 0, 16, 16],
    rotation: 0,
    cullface: null,
    tintindex: null,
    missing: true,
    searchedSources: ["demo"],
  };
  const faces = Object.fromEntries(
    ["down", "up", "north", "south", "west", "east"].map((name) => [name, missingFace]),
  ) as ResolvedElement["faces"];
  const model: ResolvedModel = {
    model: "demo:block/missing",
    source: asset("assets/demo/models/block/missing.json", "block_model"),
    sourceChain: [asset("assets/demo/models/block/missing.json", "block_model")],
    textures: { missing: { ...textureReference("demo:block/missing", null), missing: true } },
    elements: [{ from: [0, 0, 0], to: [16, 16, 16], shade: true, faces }],
    ambientOcclusion: true,
    display: {},
    guiLight: "side",
    generatedItem: false,
  };
  return {
    kind: "block",
    manifest: {
      translationKey: "block.demo.missing",
      variants: [],
      selectedVariant: null,
      properties: [],
      selectedState: {},
      models: [{ model, xRotation: 0, yRotation: 0, uvlock: false }],
    },
  };
}

function element(from: [number, number, number], to: [number, number, number], location: AssetLocation): ResolvedElement {
  const face: ResolvedFace = {
    texture: "demo:block/cube_all",
    texturePath: location,
    uv: [0, 0, 16, 16],
    rotation: 0,
    cullface: null,
    tintindex: null,
    missing: false,
    searchedSources: ["demo"],
  };
  const faces = Object.fromEntries(
    ["down", "up", "north", "south", "west", "east"].map((name) => [name, { ...face }]),
  ) as ResolvedElement["faces"];
  return { from, to, shade: true, faces };
}

function textureReference(location: string, path: AssetLocation | null): TextureReference {
  return { location, path, missing: false, searchedSources: ["demo"] };
}

function asset(archivePath: string, assetType: AssetLocation["assetType"]): AssetLocation {
  return {
    sourceId: "preview-demo",
    sourceKind: "builtin",
    archivePath,
    namespace: "demo",
    resourceId: archivePath.split("/").at(-1)?.replace(/\.png$/, "") ?? archivePath,
    assetType,
  };
}

async function createDemoTextureBytes(): Promise<Map<string, Uint8Array>> {
  const grass = createPixelCanvas((context) => {
    const colors = ["#688b3d", "#769d45", "#88ad50", "#5b7d36"];
    for (let y = 0; y < 16; y += 1) {
      for (let x = 0; x < 16; x += 1) {
        context.fillStyle = colors[(x * 3 + y * 5 + (x ^ y)) % colors.length];
        context.fillRect(x, y, 1, 1);
      }
    }
    context.fillStyle = "#a2c65b";
    context.fillRect(2, 3, 3, 2);
  });
  const generated = createPixelCanvas((context) => {
    const pixels: Array<[number, number, string]> = [
      [7, 0, "#f1d469"], [8, 0, "#f1d469"], [6, 1, "#e9c550"], [7, 1, "#e9c550"], [8, 1, "#e9c550"], [9, 1, "#e9c550"],
      [6, 2, "#d3a83d"], [7, 2, "#f0d15f"], [8, 2, "#f0d15f"], [9, 2, "#d3a83d"], [7, 3, "#e9c550"], [8, 3, "#e9c550"],
      [7, 4, "#7c542e"], [8, 4, "#7c542e"], [7, 5, "#906336"], [8, 5, "#906336"], [6, 6, "#a6733c"], [7, 6, "#a6733c"], [8, 6, "#a6733c"], [9, 6, "#a6733c"],
      [5, 7, "#966a3c"], [6, 7, "#966a3c"], [7, 7, "#966a3c"], [8, 7, "#966a3c"], [9, 7, "#966a3c"], [10, 7, "#966a3c"],
      [4, 8, "#855d38"], [5, 8, "#855d38"], [6, 8, "#855d38"], [7, 8, "#855d38"], [8, 8, "#855d38"], [9, 8, "#855d38"], [10, 8, "#855d38"], [11, 8, "#855d38"],
      [3, 9, "#684a30"], [4, 9, "#684a30"], [5, 9, "#684a30"], [6, 9, "#684a30"], [7, 9, "#684a30"], [8, 9, "#684a30"], [9, 9, "#684a30"], [10, 9, "#684a30"], [11, 9, "#684a30"], [12, 9, "#684a30"],
      [5, 10, "#d9d9d9"], [6, 10, "#d9d9d9"], [7, 10, "#d9d9d9"], [8, 10, "#d9d9d9"], [9, 10, "#d9d9d9"], [10, 10, "#d9d9d9"],
      [6, 11, "#efefef"], [7, 11, "#efefef"], [8, 11, "#efefef"], [9, 11, "#efefef"], [7, 12, "#bfc3c4"], [8, 12, "#bfc3c4"],
      [7, 13, "#f2f2f2"], [8, 13, "#f2f2f2"], [7, 14, "#bfc3c4"], [8, 14, "#bfc3c4"],
    ];
    for (const [x, y, color] of pixels) {
      context.fillStyle = color;
      context.fillRect(x, y, 1, 1);
    }
  });

  const [grassBytes, itemBytes] = await Promise.all([canvasBytes(grass), canvasBytes(generated)]);
  return new Map([
    [`preview-demo:${GRASS_ASSET.archivePath}`, grassBytes],
    [`preview-demo:${ITEM_ASSET.archivePath}`, itemBytes],
  ]);
}

function createPixelCanvas(draw: (context: CanvasRenderingContext2D) => void): HTMLCanvasElement {
  const canvas = document.createElement("canvas");
  canvas.width = 16;
  canvas.height = 16;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("Canvas is unavailable");
  context.clearRect(0, 0, 16, 16);
  draw(context);
  return canvas;
}

function canvasBytes(canvas: HTMLCanvasElement): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (!blob) {
        reject(new Error("Demo texture could not be encoded"));
        return;
      }
      void blob.arrayBuffer().then((buffer) => resolve(new Uint8Array(buffer)), reject);
    }, "image/png");
  });
}
