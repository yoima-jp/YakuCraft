import { t } from "../../i18n";

export function unitName(unit: { sourceKind?: "jar" | "modpack" | "shaderpack"; name: string | null; sourceFilename: string }) {
  if (unit.sourceKind === "modpack") return t("editor.modpackSpecific");
  if (unit.sourceKind === "shaderpack") return unit.sourceFilename.replace(/\.zip$/i, "");
  return unit.name?.trim() || unit.sourceFilename;
}
