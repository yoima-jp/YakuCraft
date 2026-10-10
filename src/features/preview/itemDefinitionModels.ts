import type { ItemPreviewManifest, ResolvedItemDefinition, ResolvedModel } from "../../types/preview";

/** Resolverが選択済みのdefinition treeをたどり、同時表示するmodelだけを返す。 */
export function activeItemModels(manifest: ItemPreviewManifest): ResolvedModel[] {
  // Legacy itemはserdeのskip_serializing_ifによりdefinition自体が欠落する。
  if (manifest.definition === null || manifest.definition === undefined) return [manifest.model];

  const indices: number[] = [];
  collectModelIndices(manifest.definition, indices);
  return indices.flatMap((index) => {
    const model = manifest.models[index];
    return model ? [model] : [];
  });
}

function collectModelIndices(definition: ResolvedItemDefinition, output: number[]): void {
  switch (definition.kind) {
    case "model":
      output.push(definition.modelIndex);
      break;
    case "composite":
      definition.children.forEach((child) => collectModelIndices(child, output));
      break;
    case "condition":
    case "select":
    case "rangeDispatch":
      collectModelIndices(definition.child, output);
      break;
    case "empty":
      break;
  }
}
