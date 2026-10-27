// @ts-check

import { spawn, spawnSync } from "node:child_process";
import { existsSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const e2eDirectory = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(e2eDirectory, "..");
const driverPidFile = process.env.YAKUCRAFT_E2E_DRIVER_PID_FILE;
const webDriverPort = Number(process.env.YAKUCRAFT_E2E_WEBDRIVER_PORT ?? 4444);
const nativeDriverPort = Number(process.env.YAKUCRAFT_E2E_NATIVE_PORT ?? webDriverPort + 1);
const defaultApplication = process.platform === "win32" ? "yakucraft.exe" : "yakucraft";
/** @type {import("node:child_process").ChildProcess | undefined} */
let tauriDriver;

// tauri-driver は --native-driver を PATH 検索しないため、実行ファイルを絶対パスに解決する。
function resolveLinuxNativeDriver() {
  const configured = process.env.YAKUCRAFT_WEBKITWEBDRIVER ?? "WebKitWebDriver";
  if (path.isAbsolute(configured)) return configured;
  if (configured.includes(path.sep)) return path.resolve(configured);

  for (const directory of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!directory) continue;
    const candidate = path.resolve(directory, configured);
    if (existsSync(candidate)) return candidate;
  }
  throw new Error(`${configured} が PATH に見つかりません。`);
}

async function waitForDriver() {
  const driver = tauriDriver;
  if (!driver) throw new Error("tauri-driver が起動していません。");
  const timeoutAt = Date.now() + 15_000;
  while (Date.now() < timeoutAt) {
    if (driver.exitCode !== null) {
      throw new Error(`tauri-driver が起動時に終了しました (code ${driver.exitCode})`);
    }
    try {
      const response = await fetch(`http://127.0.0.1:${webDriverPort}/status`);
      if (response.ok) return;
    } catch {
      // WebDriver serverのlisten完了を短い間隔で確認する。
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("tauri-driver が15秒以内にWebDriver serverを開始しませんでした。");
}

async function stopTauriDriver() {
  const driver = tauriDriver;
  if (!driver?.pid || driver.exitCode !== null) return;
  if (process.platform === "win32") {
    spawnSync("taskkill.exe", ["/PID", String(driver.pid), "/T", "/F"], { stdio: "ignore" });
  } else {
    driver.kill("SIGTERM");
  }
  /** @type {Promise<void>} */
  const driverStopped = new Promise((resolve) => {
    if (driver.exitCode !== null) return resolve();
    const timeout = setTimeout(resolve, 5_000);
    driver.once("exit", () => {
      clearTimeout(timeout);
      resolve();
    });
  });
  await driverStopped;
  if (driverPidFile && existsSync(driverPidFile)) unlinkSync(driverPidFile);
  tauriDriver = undefined;
}

/** @type {import("@wdio/types").Capabilities.RequestedStandaloneCapabilities & { "tauri:options": { application: string } }} */
const tauriCapabilities = {
  ...(process.platform === "linux" ? { "wdio:enforceWebDriverClassic": true } : {}),
  "tauri:options": {
    application: process.env.YAKUCRAFT_E2E_APPLICATION
      ?? path.join(projectRoot, "target", "debug", defaultApplication),
  },
};

/** @type {import("@wdio/types").Options.Testrunner & import("@wdio/types").Capabilities.WithRequestedTestrunnerCapabilities} */
export const config = {
  logLevel: "warn",
  runner: "local",
  // YAKUCRAFT_E2E_SPEC は、実在の Mod など第三者のファイルを使う手元専用の spec を
  // リポジトリの外に置いたまま流すための指定。CI と通常の test:e2e では使わない。
  specs: process.env.YAKUCRAFT_E2E_SPEC
    ? [path.resolve(process.env.YAKUCRAFT_E2E_SPEC)]
    : process.env.YAKUCRAFT_E2E_SCREENSHOTS === "1"
    ? [path.join(e2eDirectory, "specs", "screenshots.spec.mjs")]
    : [
      ...(process.platform === "linux"
        ? [path.join(e2eDirectory, "specs", "linux-runtime.spec.mjs")]
        : []),
      path.join(e2eDirectory, "specs", "first-milestone.spec.mjs"),
      path.join(e2eDirectory, "specs", "mod-tab-reorder.spec.mjs"),
      path.join(e2eDirectory, "specs", "translation-scroll.spec.mjs"),
      path.join(e2eDirectory, "specs", "recent-project-loading.spec.mjs"),
      path.join(e2eDirectory, "specs", "project-delete.spec.mjs"),
      path.join(e2eDirectory, "specs", "modpack-flow.spec.mjs"),
      path.join(e2eDirectory, "specs", "full-feature-verification.spec.mjs"),
    ],
  maxInstances: 1,
  hostname: "127.0.0.1",
  port: webDriverPort,
  capabilities: [tauriCapabilities],
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    timeout: 240_000,
  },
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 1,
  async beforeSession() {
    const tauriDriverPath = process.env.YAKUCRAFT_TAURI_DRIVER;
    const nativeDriverPath = process.platform === "win32"
      ? process.env.YAKUCRAFT_MSEDGEDRIVER
      : process.platform === "linux"
      ? resolveLinuxNativeDriver()
      : undefined;
    if (!tauriDriverPath || !nativeDriverPath) {
      throw new Error("E2E driver の実行パスが設定されていません。");
    }
    tauriDriver = spawn(tauriDriverPath, [
      "--port", String(webDriverPort),
      "--native-port", String(nativeDriverPort),
      "--native-driver", nativeDriverPath,
    ], { stdio: "inherit" });
    if (driverPidFile) writeFileSync(driverPidFile, String(tauriDriver.pid), "utf8");
    tauriDriver.once("error", (error) => console.error("tauri-driver error:", error));
    await waitForDriver();
  },
  async afterSession() {
    await stopTauriDriver();
  },
};

process.once("exit", () => {
  if (tauriDriver?.pid && tauriDriver.exitCode === null && process.platform === "win32") {
    spawnSync("taskkill.exe", ["/PID", String(tauriDriver.pid), "/T", "/F"], { stdio: "ignore" });
  } else if (tauriDriver?.pid && tauriDriver.exitCode === null && process.platform !== "win32") {
    try {
      process.kill(tauriDriver.pid, "SIGTERM");
    } catch (error) {
      if (!(error instanceof Error) || !("code" in error) || error.code !== "ESRCH") throw error;
    }
  }
});
