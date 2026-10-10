import type { ResolvedItemDefinition } from "../../types/preview";

export const DEFAULT_GRASS_TINT = 0x91bd59;

/** Item definition treeの選択済みmodel順に、静的に再現できるtintを並べる。 */
export function activeItemTintColors(
  definition: ResolvedItemDefinition | null,
): Array<Array<number | null>> {
  if (!definition) return [];
  const output: Array<Array<number | null>> = [];

  const visit = (node: ResolvedItemDefinition) => {
    switch (node.kind) {
      case "model":
        output.push(node.tints.map(resolveItemTint));
        break;
      case "composite":
        node.children.forEach(visit);
        break;
      case "condition":
      case "select":
      case "rangeDispatch":
        visit(node.child);
        break;
      case "empty":
        break;
    }
  };
  visit(definition);
  return output;
}

function resolveItemTint(value: unknown): number | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const tint = value as { type?: unknown; value?: unknown; default?: unknown };
  if (tint.type === "minecraft:grass") return DEFAULT_GRASS_TINT;

  if (![
    "minecraft:constant",
    "minecraft:dye",
    "minecraft:potion",
    "minecraft:firework",
    "minecraft:map_color",
  ].includes(String(tint.type))) {
    return null;
  }
  const color = typeof tint.value === "number" ? tint.value : tint.default;
  return typeof color === "number" && Number.isFinite(color) ? color & 0xffffff : null;
}
