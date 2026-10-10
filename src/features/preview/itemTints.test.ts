import { describe, expect, it } from "vitest";
import { activeItemTintColors, DEFAULT_GRASS_TINT } from "./itemTints";

describe("item definition tint colors", () => {
  it("uses static and default colors in model order and the plains grass color", () => {
    const colors = activeItemTintColors({
      kind: "composite",
      children: [
        {
          kind: "model",
          modelIndex: 0,
          tints: [
            { type: "minecraft:constant", value: -16711936 },
            { type: "minecraft:dye", default: -6265536 },
            { type: "minecraft:potion", default: 0x44aaff },
            { type: "minecraft:grass", downfall: 1, temperature: 0.5 },
          ],
        },
        {
          kind: "model",
          modelIndex: 1,
          tints: [{ type: "minecraft:custom_model_data" }],
        },
      ],
    });

    expect(colors).toEqual([
      [0x00ff00, 0xa06540, 0x44aaff, DEFAULT_GRASS_TINT],
      [null],
    ]);
  });

  it("uses the built-in grass color for an unselected legacy definition", () => {
    expect(activeItemTintColors(null)).toEqual([]);
    expect(DEFAULT_GRASS_TINT).toBe(0x91bd59);
  });
});
