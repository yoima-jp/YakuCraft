export function unitName(unit: { sourceKind?: "jar" | "modpack" | "shaderpack"; name: string | null; sourceFilename: string }) {
  if (unit.sourceKind === "modpack") return "Modpack 固有（クエスト・KubeJS）";
  if (unit.sourceKind === "shaderpack") return unit.sourceFilename.replace(/\.zip$/i, "");
  return unit.name?.trim() || unit.sourceFilename;
}
