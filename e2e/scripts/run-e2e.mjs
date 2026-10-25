import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
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
  };

  try {
    if (process.env.APPIMAGE) {
      await mkdir(dataDirectory, { recursive: true });
      await writeFile(
        path.join(dataDirectory, "settings.json"),
        JSON.stringify({ schemaVersion: 1, "updates.checkOnStartup": false }),
        "utf8",
      );
    }

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
