import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { copyFile, cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const e2eDirectory = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const projectRoot = path.resolve(e2eDirectory, "..");
const cargoBin = path.join(process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"), "bin");
const cargoCommand = process.platform === "win32" ? path.join(cargoBin, "cargo.exe") : "cargo";
const tauriDriverPath = path.join(cargoBin, process.platform === "win32" ? "tauri-driver.exe" : "tauri-driver");
const screenshotsOnly = process.argv.includes("--screenshots");
// PLaMo の実機E2Eは実モデル（数百MB）とGPU/CPUのランタイムを使うため、明示したときだけ流す。
// `--plamo` は spec の選択と、開発用キャッシュからのシード可否をまとめて切り替える。
const plamoOnly = process.argv.includes("--plamo") || process.env.YAKUCRAFT_E2E_PLAMO === "1";
const plamoSeed = plamoOnly && process.env.YAKUCRAFT_E2E_PLAMO_SEED === "1";
let childProcess;

function run(command, args, { cwd = projectRoot, env = process.env, shell = false } = {}) {
  return new Promise((resolve, reject) => {
    childProcess = spawn(command, args, { cwd, env, shell, stdio: "inherit" });
    childProcess.once("error", reject);
    childProcess.once("exit", (code, signal) => {
      childProcess = undefined;
      if (code === 0) resolve();
      else reject(new Error(`${command} が失敗しました (code=${code ?? "none"}, signal=${signal ?? "none"})`));
    });
  });
}

function runNpm(args, env) {
  if (process.env.npm_execpath) {
    return run(process.execPath, [process.env.npm_execpath, ...args], { env });
  }
  return run(process.platform === "win32" ? "npm.cmd" : "npm", args, {
    env,
    shell: process.platform === "win32",
  });
}

function runCapture(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: projectRoot,
    encoding: "utf8",
    windowsHide: true,
    ...options,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} が失敗しました: ${result.stderr || result.stdout}`);
  }
  return result.stdout.trim();
}

function findAvailableWebDriverPort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("空いている WebDriver ポートを取得できませんでした。"));
        return;
      }
      server.close((error) => {
        if (error) reject(error);
        else resolve(address.port);
      });
    });
  });
}

async function assertProductionBundleHasNoE2EHook() {
  const bundleRoot = path.join(projectRoot, "dist");
  const marker = Buffer.from("__YAKUCRAFT_E2E__");
  const pending = [bundleRoot];
  while (pending.length > 0) {
    const directory = pending.pop();
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const entryPath = path.join(directory, entry.name);
      if (entry.isDirectory()) pending.push(entryPath);
      else if ((await readFile(entryPath)).includes(marker)) {
        throw new Error(`通常ビルドにE2Eダイアログ差し替えが含まれています: ${entryPath}`);
      }
    }
  }
  console.log("通常ビルドにE2E用window参照がないことを確認しました。");
}

/**
 * PLaMo の実機検証を繰り返すためのシード。開発用キャッシュのモデルとランタイムを、
 * `src-tauri/src/plamo.rs` が期待するレイアウト（`<data>/plamo/models`・`<data>/plamo/runtime`）
 * へコピーし、ライセンス同意も保存する。初回の実ダウンロードは UI から通す前提で、
 * これは 2 回目以降の短縮用。
 */
async function seedPlamoFiles(dataDirectory, devCachePath, settingsFile) {
  const modelsDir = path.join(dataDirectory, "plamo", "models");
  const runtimeDir = path.join(dataDirectory, "plamo", "runtime");
  await mkdir(modelsDir, { recursive: true });
  await mkdir(runtimeDir, { recursive: true });

  const modelFile = "plamo-3-translate-0.8b-Q4_K_M.gguf";
  const modelCandidates = [
    path.join(devCachePath, "models", modelFile),
    path.join(devCachePath, "yc-llama-verify", "models", modelFile),
  ];
  const modelSource = modelCandidates.find((candidate) => existsSync(candidate));
  if (modelSource) {
    await copyFile(modelSource, path.join(modelsDir, modelFile));
    console.log(`PLaMo モデルをシードしました: ${modelSource}`);
  } else {
    console.warn(`PLaMo モデルが見つからないためシードを省略します: ${modelFile}`);
  }

  // ランタイムは展開済みディレクトリをそのまま持ってくる（`b12110-cpu` / `b12110-vulkan`）。
  for (const installDirName of ["b12110-cpu", "b12110-vulkan"]) {
    const candidates = [
      path.join(devCachePath, "yc-llama-verify", "runtimes", installDirName),
      path.join(devCachePath, "runtimes", installDirName),
    ];
    const source = candidates.find((candidate) => existsSync(candidate));
    if (!source) {
      console.warn(`PLaMo ランタイムが見つからないためシードを省略します: ${installDirName}`);
      continue;
    }
    await cp(source, path.join(runtimeDir, installDirName), { recursive: true });
    console.log(`PLaMo ランタイムをシードしました: ${source}`);
  }

  // ファイルを置いただけでは実行できない。ライセンス同意と選択も保存し、
  // シード後に UI のダウンロードを経ずに翻訳を始められる状態にする。
  const settings = JSON.parse(await readFile(settingsFile, "utf8"));
  Object.assign(settings, {
    "ai.provider": "plamo",
    "ai.plamo.model": "plamo-3-translate-0.8b",
    "ai.plamo.device": "gpu",
    "ai.plamo.licenseAccepted": true,
  });
  await writeFile(settingsFile, JSON.stringify(settings), "utf8");
  console.log("PLaMo の設定（モデル・デバイス・ライセンス同意）をシードしました。");
}

async function terminateDriverFromPidFile(pidFile) {
  if (!existsSync(pidFile)) return;
  const processId = Number(readFileSync(pidFile, "utf8").trim());
  if (Number.isInteger(processId) && processId > 0 && process.platform === "win32") {
    spawnSync("taskkill.exe", ["/PID", String(processId), "/T", "/F"], { stdio: "ignore" });
  } else if (Number.isInteger(processId) && processId > 0 && process.platform !== "win32") {
    try {
      process.kill(processId, "SIGTERM");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  }
  rmSync(pidFile, { force: true });
}

async function main() {
  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "yakucraft-e2e-"));
  const fixturePath = path.join(temporaryRoot, "analysis-fixture.jar");
  const largeFixturePath = path.join(temporaryRoot, "analysis-fixture-500.jar");
  const darkFixturePath = path.join(temporaryRoot, "analysis-fixture-500-dark.jar");
  const exportPath = path.join(temporaryRoot, "ja_jp.json");
  const dataDirectory = path.join(temporaryRoot, "app-data");
  const driverPidFile = path.join(temporaryRoot, "tauri-driver.pid");
  // PLaMo の spec は実 Mod（Farmer's Delight）と、任意で開発用キャッシュのモデル・ランタイムを使う。
  const devCachePath = process.env.YAKUCRAFT_E2E_PLAMO_DEV_CACHE
    ?? path.join(process.env.LOCALAPPDATA ?? os.homedir(), "YakuCraft-dev-cache");
  const plamoModJarPath = process.env.YAKUCRAFT_E2E_PLAMO_MOD_JAR
    ?? path.join(devCachePath, "mods", "FarmersDelight-1.20.1-1.3.4.jar");
  const baseEnvironment = { ...process.env };
  delete baseEnvironment.VITE_E2E;
  delete baseEnvironment.YAKUCRAFT_DATA_DIR;
  const testEnvironment = {
    ...baseEnvironment,
    VITE_E2E: "1",
    YAKUCRAFT_E2E: "1",
    YAKUCRAFT_DATA_DIR: dataDirectory,
    YAKUCRAFT_E2E_FIXTURE: fixturePath,
    YAKUCRAFT_E2E_LARGE_FIXTURE: largeFixturePath,
    YAKUCRAFT_E2E_DARK_FIXTURE: darkFixturePath,
    YAKUCRAFT_E2E_EXPORT: exportPath,
    YAKUCRAFT_E2E_DRIVER_PID_FILE: driverPidFile,
    YAKUCRAFT_E2E_SCREENSHOTS: screenshotsOnly ? "1" : "0",
    YAKUCRAFT_E2E_PLAMO: plamoOnly ? "1" : "0",
    YAKUCRAFT_E2E_PLAMO_SEED: plamoSeed ? "1" : "0",
    ...(plamoOnly ? { YAKUCRAFT_E2E_PLAMO_MOD_JAR: plamoModJarPath, YAKUCRAFT_E2E_PLAMO_DEV_CACHE: devCachePath } : {}),
  };

  try {
    await mkdir(dataDirectory, { recursive: true });
    const settingsFile = path.join(dataDirectory, "settings.json");
    await writeFile(
      settingsFile,
      JSON.stringify({
        schemaVersion: 1,
        "updates.checkOnStartup": false,
        "onboarding.completed": true,
        "language.uiLanguage": "ja",
        "language.targetLocale": "ja_jp",
      }),
      "utf8",
    );
    if (plamoSeed) await seedPlamoFiles(dataDirectory, devCachePath, settingsFile);

    await run(cargoCommand, ["run", "-p", "yc-analysis", "--example", "make_fixture", "--", fixturePath]);
    await run(cargoCommand, ["run", "-p", "yc-analysis", "--example", "make_fixture", "--", largeFixturePath, "--keys", "500"]);
    await copyFile(largeFixturePath, darkFixturePath);

    await runNpm(["run", "build"], baseEnvironment);
    await assertProductionBundleHasNoE2EHook();

    let nativeDriverPath;
    if (process.platform === "win32") {
      const powershell = process.env.SystemRoot
        ? path.join(process.env.SystemRoot, "System32", "WindowsPowerShell", "v1.0", "powershell.exe")
        : "powershell.exe";
      nativeDriverPath = runCapture(powershell, [
        "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
        "-File", path.join(e2eDirectory, "scripts", "prepare-msedgedriver.ps1"),
      ]);
    } else if (process.platform === "linux") {
      nativeDriverPath = process.env.YAKUCRAFT_WEBKITWEBDRIVER ?? "WebKitWebDriver";
    } else {
      throw new Error("このE2E構成はWindowsまたはLinuxが必要です。");
    }

    if (!existsSync(tauriDriverPath)) {
      await run(cargoCommand, ["install", "tauri-driver", "--locked"]);
    }
    if (!existsSync(tauriDriverPath)) {
      throw new Error("tauri-driver をインストールできませんでした。");
    }

    const cargoMetadata = JSON.parse(runCapture(cargoCommand, ["metadata", "--no-deps", "--format-version", "1"]));
    const applicationPath = path.join(
      cargoMetadata.target_directory,
      "debug",
      process.platform === "win32" ? "yakucraft.exe" : "yakucraft",
    );
    await runNpm(["run", "tauri", "--", "build", "--debug", "--no-bundle"], {
      ...testEnvironment,
      PATH: `${cargoBin}${path.delimiter}${process.env.PATH ?? ""}`,
      YAKUCRAFT_TAURI_DRIVER: tauriDriverPath,
      ...(process.platform === "win32"
        ? { YAKUCRAFT_MSEDGEDRIVER: nativeDriverPath }
        : { YAKUCRAFT_WEBKITWEBDRIVER: nativeDriverPath }),
      YAKUCRAFT_E2E_APPLICATION: applicationPath,
    });
    if (!existsSync(applicationPath)) throw new Error(`E2Eアプリがありません: ${applicationPath}`);

    // 共用 Windows 環境で固定ポート 4444 の WebDriver と衝突しないよう、空きポートを選ぶ。
    const webDriverPort = await findAvailableWebDriverPort();
    await runNpm(["exec", "--", "wdio", "run", path.join(e2eDirectory, "wdio.conf.mjs")], {
      ...testEnvironment,
      YAKUCRAFT_E2E_WEBDRIVER_PORT: String(webDriverPort),
      YAKUCRAFT_TAURI_DRIVER: tauriDriverPath,
      ...(process.platform === "win32"
        ? { YAKUCRAFT_MSEDGEDRIVER: nativeDriverPath }
        : { YAKUCRAFT_WEBKITWEBDRIVER: nativeDriverPath }),
      YAKUCRAFT_E2E_APPLICATION: applicationPath,
    });
  } finally {
    if (childProcess?.pid) {
      if (process.platform === "win32") {
        spawnSync("taskkill.exe", ["/PID", String(childProcess.pid), "/T", "/F"], { stdio: "ignore" });
      } else {
        childProcess.kill("SIGTERM");
      }
    }
    await terminateDriverFromPidFile(driverPidFile);
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
