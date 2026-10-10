import { readFileSync } from "node:fs";

function readTomlField(source, section, field) {
  const sectionPattern = new RegExp(`(?:^|\\n)\\[${section.replaceAll(".", "\\.")}\\]\\s*\\n([\\s\\S]*?)(?=\\n\\[|$)`);
  const body = source.match(sectionPattern)?.[1];
  if (!body) throw new Error(`Cargo.toml に [${section}] がありません。`);
  const escapedField = field.replaceAll(".", "\\.").replaceAll("-", "\\-");
  const fieldPattern = new RegExp(`^${escapedField}\\s*=\\s*(?:"([^"]+)"|(true|false))\\s*$`, "m");
  const match = body.match(fieldPattern);
  if (!match) throw new Error(`Cargo.toml の [${section}] に ${field} がありません。`);
  return match[1] ?? match[2];
}

function readCargoPackageVersion(source, workspaceVersion) {
  try {
    return readTomlField(source, "package", "version");
  } catch {
    const inheritedVersion = readTomlField(source, "package", "version.workspace");
    if (inheritedVersion === "true") return workspaceVersion;
    throw new Error("src-tauri/Cargo.toml の package.version が正しくありません。");
  }
}

const packageJson = JSON.parse(readFileSync("package.json", "utf8"));
const tauriConfig = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
const workspaceToml = readFileSync("Cargo.toml", "utf8");
const tauriToml = readFileSync("src-tauri/Cargo.toml", "utf8");
const workspaceVersion = readTomlField(workspaceToml, "workspace.package", "version");
const versions = {
  "package.json": packageJson.version,
  "Cargo.toml [workspace.package]": workspaceVersion,
  "src-tauri/Cargo.toml": readCargoPackageVersion(tauriToml, workspaceVersion),
  "tauri.conf.json": tauriConfig.version,
};

const uniqueVersions = new Set(Object.values(versions));
if (uniqueVersions.size !== 1) {
  console.error("バージョンが一致しません:");
  for (const [file, version] of Object.entries(versions)) console.error(`  ${file}: ${version ?? "未設定"}`);
  process.exitCode = 1;
} else {
  const [version] = uniqueVersions;
  const releaseTag = process.env.YAKUCRAFT_RELEASE_TAG;
  if (releaseTag && releaseTag !== `v${version}`) {
    console.error(`タグ ${releaseTag} とアプリのバージョン v${version} が一致しません。`);
    process.exitCode = 1;
  } else {
    console.log(`バージョン ${version} を確認しました。`);
  }
}
