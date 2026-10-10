import { spawn, spawnSync } from "node:child_process";
import { existsSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const e2eDirectory = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(e2eDirectory, "..");
const driverPidFile = process.env.YAKUCRAFT_E2E_DRIVER_PID_FILE;
const webDriverPort = Number(process.env.YAKUCRAFT_E2E_WEBDRIVER_PORT ?? 4444);
const nativeDriverPort = Number(process.env.YAKUCRAFT_E2E_NATIVE_PORT ?? webDriverPort + 1);
let tauriDriver;

async function waitForDriver() {
  const timeoutAt = Date.now() + 15_000;
  while (Date.now() < timeoutAt) {
    if (tauriDriver.exitCode !== null) {
      throw new Error(`tauri-driver が起動時に終了しました (code ${tauriDriver.exitCode})`);
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
  if (!tauriDriver?.pid || tauriDriver.exitCode !== null) return;
  if (process.platform === "win32") {
    spawnSync("taskkill.exe", ["/PID", String(tauriDriver.pid), "/T", "/F"], { stdio: "ignore" });
  } else {
    tauriDriver.kill("SIGTERM");
  }
  await new Promise((resolve) => {
    if (tauriDriver.exitCode !== null) return resolve();
    const timeout = setTimeout(resolve, 5_000);
    tauriDriver.once("exit", () => {
      clearTimeout(timeout);
      resolve();
    });
  });
  if (driverPidFile && existsSync(driverPidFile)) unlinkSync(driverPidFile);
  tauriDriver = undefined;
}

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
      path.join(e2eDirectory, "specs", "first-milestone.spec.mjs"),
      path.join(e2eDirectory, "specs", "mod-tab-reorder.spec.mjs"),
      path.join(e2eDirectory, "specs", "translation-scroll.spec.mjs"),
      path.join(e2eDirectory, "specs", "recent-project-loading.spec.mjs"),
      path.join(e2eDirectory, "specs", "project-delete.spec.mjs"),
      path.join(e2eDirectory, "specs", "modpack-flow.spec.mjs"),
    ],
  maxInstances: 1,
  hostname: "127.0.0.1",
  port: webDriverPort,
  capabilities: [
    {
      "tauri:options": {
        application: process.env.YAKUCRAFT_E2E_APPLICATION
          ?? path.join(projectRoot, "target", "debug", "yakucraft.exe"),
      },
    },
  ],
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    timeout: 240_000,
  },
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 1,
  async beforeSession() {
    const tauriDriverPath = process.env.YAKUCRAFT_TAURI_DRIVER;
    const edgeDriverPath = process.env.YAKUCRAFT_MSEDGEDRIVER;
    if (!tauriDriverPath || !edgeDriverPath) {
      throw new Error("E2E driver の実行パスが設定されていません。");
    }
    tauriDriver = spawn(tauriDriverPath, [
      "--port", String(webDriverPort),
      "--native-port", String(nativeDriverPort),
      "--native-driver", edgeDriverPath,
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
  }
});
