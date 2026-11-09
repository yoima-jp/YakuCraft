// PLaMo（ローカル翻訳）の実機E2E。
//
// 実モデル（0.8B で約 573MB）と実ランタイム（b12110 の Vulkan / CPU ビルド）を使い、
// 実 Mod（Farmer's Delight / Create）を翻訳するため、通常の `npm run test:e2e` では流さない。
// `YAKUCRAFT_E2E_PLAMO=1`（または `npm run test:e2e -- --plamo`）のときだけ実行する。
//
// 2 回目以降は `YAKUCRAFT_E2E_PLAMO_SEED=1` で開発用キャッシュのモデルとランタイムを
// 隔離データディレクトリへコピーし、ダウンロードの待ち時間を省ける。初回はUIからの
// 実ダウンロード（ライセンス同意 → 進捗 → 中断 → 再開 → 完了）を通す。

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { chmod, mkdir, readFile, readdir, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { readZipEntries } from "../scripts/generate-feature-fixtures.mjs";

const enabled = process.env.YAKUCRAFT_E2E_PLAMO === "1";
const describePlamo = enabled ? describe : describe.skip;

const screenshotsRoot = fileURLToPath(new URL("../screenshots/plamo/", import.meta.url));
const dataDirectory = process.env.YAKUCRAFT_DATA_DIR ?? "";
const modJarPath = process.env.YAKUCRAFT_E2E_PLAMO_MOD_JAR ?? "";
const createJarPath = modJarPath.replace(/FarmersDelight[^\\/]*\.jar$/, "create-1.20.1-6.0.8.jar");
const fixtureJarPath = process.env.YAKUCRAFT_E2E_FIXTURE ?? "";
const seeded = process.env.YAKUCRAFT_E2E_PLAMO_SEED === "1";

const PLAMO_MODEL_FILE = "plamo-3-translate-0.8b-Q4_K_M.gguf";
const PLAMO_MODEL_BYTES = 573_126_848;
const PERCENT_KEY = "container.farmersdelight.cooking_pot.served_on";
const SECTION_KEY = "advancement.create.belt_funnel_kiss.desc";
const GPU_KEY = "create.command.killTPSCommand.status.slowed_by.0";
const CPU_KEY = "create.command.killTPSCommand.status.slowed_by.1";
const NOTICE_FILE = "PLaMo_NOTICE.txt";

/** 画面の文言（日本語UI）。 */
const text = {
  settings: "設定",
  save: "保存",
  testConnection: "接続テスト",
  delete: "削除",
  cancel: "キャンセル",
  aiCategory: "AI翻訳",
  appearanceCategory: "表示",
  languageCategory: "言語",
  downloadPrefix: "ダウンロード",
  licenseAccept: "同意してダウンロード",
  progressAria: "ダウンロードの進行状況",
  privacyTitle: "AI翻訳の送信内容を確認",
  overwriteTitle: "既存の翻訳を上書きしますか？",
  overwrite: "上書き",
  translate: "翻訳",
  exported: "書き出しました",
  saved: "保存しました",
  connected: "接続できました",
};

async function waitForText(selector, expectedText, timeout = 30_000) {
  const element = await $(selector);
  await element.waitForDisplayed({ timeout });
  await browser.waitUntil(async () => (await element.getText()) === expectedText, {
    timeout,
    interval: 100,
    timeoutMsg: `${selector} が「${expectedText}」になりませんでした。`,
  });
  return element;
}

async function findButton(label, { prefix = false, scope = "button" } = {}) {
  for (const button of await $$(scope)) {
    if (!(await button.isDisplayed().catch(() => false))) continue;
    const value = (await button.getText()).trim();
    if (prefix ? value.startsWith(label) : value === label) return button;
  }
  return null;
}

async function clickButton(label, options = {}) {
  const button = await findButton(label, options);
  if (button) {
    await button.click();
    return button;
  }
  const visible = [];
  for (const candidate of await $$(options.scope ?? "button")) {
    if (await candidate.isDisplayed().catch(() => false)) visible.push((await candidate.getText()).trim());
  }
  assert.fail(`ボタン「${label}」がありません。表示中: ${visible.join(" | ")}`);
}

/** 設定画面を開く（既に開いていればそのまま）。 */
async function openSettings(category) {
  if (!await $(".settings-screen").isDisplayed().catch(() => false)) {
    const button = await findButton(text.settings);
    assert.ok(button, "設定を開くボタンがありません。");
    await button.click();
    await $(".settings-screen").waitForDisplayed({ timeout: 15_000 });
  }
  if (category) await selectSettingsCategory(category);
}

async function selectSettingsCategory(label) {
  for (const button of await $('[aria-label="設定カテゴリ"]').$$("button")) {
    if ((await button.getText()).trim() === label) {
      await button.click();
      await waitForText(".settings-content > h2", label);
      return;
    }
  }
  assert.fail(`設定カテゴリ「${label}」がありません。`);
}

async function closeSettings() {
  await $("footer.settings-actions button").click();
  await $(".settings-screen").waitForDisplayed({ reverse: true, timeout: 15_000 });
}

async function selectProvider(value, label) {
  await $('[role="combobox"][aria-label="プロバイダー"]').click();
  await $(`[role="option"][data-value="${value}"]`).click();
  await browser.waitUntil(async () => (
    await $('[role="combobox"][aria-label="プロバイダー"]').getText()
  ).includes(label), { timeout: 15_000, interval: 100, timeoutMsg: `プロバイダー「${label}」を選べませんでした。` });
}

async function selectOption(ariaLabel, value, expectedLabel) {
  await $(`[role="combobox"][aria-label="${ariaLabel}"]`).click();
  await $(`[role="option"][data-value="${value}"]`).click();
  await browser.waitUntil(async () => (
    await $(`[role="combobox"][aria-label="${ariaLabel}"]`).getText()
  ).includes(expectedLabel), { timeout: 10_000, interval: 100, timeoutMsg: `${ariaLabel}を「${expectedLabel}」にできませんでした。` });
}

/**
 * スクリーンショットを保存する。
 *
 * 保存先は実際に適用されているテーマで分ける。設定の既定は「システムに合わせる」なので、
 * 追跡用の変数ではなく `data-theme` を読む（読まないと OS がダークの環境で light/ に
 * ダークの画像が入る）。
 */
async function capture(name, settle = 250) {
  if (settle > 0) await browser.pause(settle);
  const applied = await browser.execute(() => document.documentElement.dataset.theme ?? "light");
  const directory = path.join(screenshotsRoot, applied);
  await mkdir(directory, { recursive: true });
  await browser.saveScreenshot(path.join(directory, `${name}.png`));
}

/** テーマを切り替え、実際に適用されるまで待つ。 */
async function setTheme(next) {
  await openSettings(text.appearanceCategory);
  await $(`[for="theme-${next}"]`).click();
  await browser.waitUntil(
    async () => await browser.execute(() => document.documentElement.dataset.theme) === next,
    { timeout: 10_000, interval: 100, timeoutMsg: `${next}テーマに切り替わりませんでした。` },
  );
}

/**
 * 設定画面をライトとダークの両方で撮る。
 *
 * 設定の既定は「システムに合わせる」なので、OS の設定によって最初のテーマが変わる。
 * 撮影の前に明示的にライトへ切り替え、`light/` と `dark/` の両方が必ず揃うようにする。
 * カテゴリを切り替えると中身が作り直されるため、開いた状態が要る画面は `prepare` で
 * 撮影の直前に開き直す。
 */
async function captureSettingsInBothThemes(name, { category = text.aiCategory, prepare } = {}) {
  const shoot = async (fileName) => {
    if (prepare) await prepare();
    await capture(fileName);
  };
  await setTheme("light");
  await openSettings(category);
  await shoot(name);
  await setTheme("dark");
  await openSettings(category);
  await shoot(`${name}-dark`);
  await setTheme("light");
  await openSettings(category);
}

/**
 * 設定以外の画面（エディター・書き出し）をライトとダークで撮る。
 *
 * テーマの切り替えは設定画面でしかできないため、切り替えのたびに設定を開いて閉じる。
 * エディターと書き出し画面は設定の背後にマウントされたままなので、閉じれば元の画面へ戻る。
 * `prepare` は撮影の直前に呼ぶ（スクロール位置は画面を開き直すたびに戻るため）。
 */
async function captureScreenInBothThemes(name, prepare) {
  const shoot = async (fileName) => {
    if (prepare) await prepare();
    await capture(fileName);
  };
  await setTheme("light");
  await closeSettings();
  await shoot(name);
  await setTheme("dark");
  await closeSettings();
  await shoot(`${name}-dark`);
  await setTheme("light");
  await closeSettings();
}

/** 隔離データディレクトリの最新ログ末尾。失敗時の原因を残すために添える。 */
async function recentAppLog(lines = 40) {
  if (!dataDirectory) return "(no data directory)";
  const logsDirectory = path.join(dataDirectory, "logs");
  if (!existsSync(logsDirectory)) return "(no logs)";
  const files = (await readdir(logsDirectory)).filter((name) => name.startsWith("yakucraft.log.")).sort();
  if (files.length === 0) return "(no log files)";
  const content = await readFile(path.join(logsDirectory, files.at(-1)), "utf8");
  return content.split("\n").slice(-lines).join("\n");
}

/**
 * この検証用データディレクトリのモデルを読み込んでいる llama-server のコマンドライン。
 *
 * 開発用キャッシュには他の作業が起動したサーバが残っていることがある。プロセスの
 * コマンドラインに隔離データディレクトリのパスが含まれるものだけを見る。
 * 実行ファイルのパスに `b12110-vulkan` / `b12110-cpu` が入るため、これが実際に
 * 動いているバックエンドの証拠になる（ログの起動行は翻訳ジョブからしか出ない）。
 */
function llamaServerCommandLines() {
  if (process.platform !== "win32") {
    const result = spawnSync("pgrep", ["-fa", "llama-server"], { encoding: "utf8" });
    return (result.stdout ?? "").split("\n")
      .map((line) => line.trim())
      .filter((line) => line !== "" && line.includes(dataDirectory));
  }
  const result = spawnSync(
    "powershell.exe",
    [
      "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
      "Get-CimInstance Win32_Process -Filter \"Name='llama-server.exe'\" | "
        + "Select-Object -ExpandProperty CommandLine",
    ],
    { encoding: "utf8", windowsHide: true },
  );
  return (result.stdout ?? "").split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "" && line.includes(dataDirectory));
}

function llamaServerProcesses() {
  return llamaServerCommandLines();
}

/** 実行中のサーバが使っているバックエンド（`gpu` = Vulkan ビルド）。 */
function runningBackend() {
  const lines = llamaServerCommandLines();
  if (lines.length === 0) return null;
  if (lines.some((line) => line.includes("b12110-cpu"))) return "cpu";
  if (lines.some((line) => line.includes("b12110-vulkan"))) return "gpu";
  return "unknown";
}

async function waitForRunningBackend(backend, timeout = 240_000) {
  await browser.waitUntil(async () => runningBackend() === backend, {
    timeout,
    interval: 500,
    timeoutMsg: `${backend} の llama-server が動いていません（実行中: ${runningBackend() ?? "なし"}）。`,
  });
}

async function plamoPartFiles() {
  if (!dataDirectory) return [];
  const roots = [path.join(dataDirectory, "plamo", "models"), path.join(dataDirectory, "plamo", "runtime")];
  const found = [];
  for (const root of roots) {
    if (!existsSync(root)) continue;
    for (const name of await readdir(root)) {
      if (name.endsWith(".part")) found.push(path.join(root, name));
    }
  }
  return found;
}

/**
 * ディレクトリへの書き込みを拒否する（Windows の ACL で WD/AD を拒否）。
 *
 * エラー表示の検証に使う。読み取り専用属性やファイルハンドルの保持では Windows の
 * 削除・作成を止められないため、実際に書き込みを失敗させる手段として ACL を使う。
 */
async function denyWrite(directory) {
  if (process.platform !== "win32") {
    await chmod(directory, 0o555);
    return;
  }
  const user = `${process.env.USERDOMAIN}\\${process.env.USERNAME}`;
  const result = spawnSync("icacls", [directory, "/deny", `${user}:(WD,AD)`], {
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(result.status, 0, `書き込み拒否を設定できませんでした: ${result.stdout}${result.stderr}`);
}

/** [`denyWrite`] を解除する。 */
async function allowWrite(directory) {
  if (process.platform !== "win32") {
    await chmod(directory, 0o755);
    return;
  }
  const user = `${process.env.USERDOMAIN}\\${process.env.USERNAME}`;
  spawnSync("icacls", [directory, "/remove:d", user], { encoding: "utf8", windowsHide: true });
}

/**
 * 翻訳表の状態フィルターを切り替える。
 *
 * 状態のセレクトはフィルターのメニューの中にあり、選ぶとメニューごと閉じる。選択の確認は
 * メニューを開き直してから行う（閉じた後にセレクトを探すと要素が無い）。
 */
async function selectFilter(label) {
  const trigger = await $(".editor-toolbar__filters > .menu__trigger");
  await trigger.waitForClickable({ timeout: 15_000 });
  const openMenu = async () => {
    if (await trigger.getAttribute("aria-expanded") === "true") return;
    await trigger.click();
    await browser.waitUntil(async () => (await trigger.getAttribute("aria-expanded")) === "true", {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "翻訳フィルターが開きませんでした。",
    });
  };
  const closeMenu = async () => {
    if (await trigger.getAttribute("aria-expanded") !== "true") return;
    await trigger.click();
    await browser.waitUntil(async () => (await trigger.getAttribute("aria-expanded")) !== "true", {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "翻訳フィルターが閉じませんでした。",
    });
  };
  const currentLabel = async () => {
    await openMenu();
    const value = (await $('[role="combobox"][aria-label="状態"]').getText()).trim();
    await closeMenu();
    return value;
  };

  if (await currentLabel() === label) return;
  await openMenu();
  await $('[role="combobox"][aria-label="状態"]').click();
  await $('[role="listbox"][aria-label="状態"]').waitForDisplayed({ timeout: 10_000 });
  let chosen = false;
  for (const option of await $$('[role="listbox"][aria-label="状態"] [role="option"]')) {
    if ((await option.getText()).trim() === label) {
      await option.click();
      chosen = true;
      break;
    }
  }
  assert.equal(chosen, true, `状態フィルター「${label}」がありません。`);
  await closeMenu();
  await browser.waitUntil(async () => (await currentLabel()) === label, {
    timeout: 10_000,
    interval: 100,
    timeoutMsg: `状態フィルターを「${label}」にできませんでした。`,
  });
}

/** 検索欄を空にし、絞り込みを解除する。テスト間で検索語が残ると行数が読めない。 */
async function clearSearch() {
  // 直前の検証が設定画面を開いたまま終わると、エディターの要素は隠れている。
  if (await $(".settings-screen").isDisplayed().catch(() => false)) await closeSettings();
  const search = await $("#translation-search");
  await search.waitForDisplayed({ timeout: 15_000 });
  if ((await search.getValue()) === "") return;
  await search.click();
  await browser.keys(["Control", "a"]);
  await browser.keys("Backspace");
  await browser.waitUntil(async () => (await search.getValue()) === "", {
    timeout: 10_000,
    interval: 100,
    timeoutMsg: "検索欄を空にできませんでした。",
  });
}

/** 現在の絞り込みで表示されている行数（ヘッダーを除く）。 */
async function visibleRowCount() {
  const value = await $('[role="grid"]').getAttribute("aria-rowcount");
  return Number(value ?? "0") - 1;
}

async function searchEditor(query) {
  const search = await $("#translation-search");
  await search.waitForDisplayed({ timeout: 15_000 });
  await search.click();
  await browser.keys(["Control", "a"]);
  await browser.keys(query);
  await browser.waitUntil(async () => (await search.getValue()) === query, {
    timeout: 10_000,
    interval: 100,
    timeoutMsg: `検索欄へ「${query}」を入力できませんでした。`,
  });
  await $(`[data-testid="translation-target-${query}"]`).waitForDisplayed({ timeout: 30_000 });
}

async function selectedCount() {
  const toolbar = await $('[role="toolbar"][aria-label="選択中の項目"]');
  if (!await toolbar.isDisplayed().catch(() => false)) return 0;
  const label = await browser.execute(() => {
    const element = document.querySelector('[role="toolbar"][aria-label="選択中の項目"] span');
    return element?.textContent ?? "";
  });
  const match = /(\d+)/.exec(label.replaceAll(",", ""));
  return match ? Number(match[1]) : 0;
}

async function translateSelection() {
  await $('[role="toolbar"][aria-label="選択中の項目"]').waitForDisplayed({ timeout: 10_000 });
  await clickButton("AIで翻訳", { scope: '[role="toolbar"][aria-label="選択中の項目"] button' });
}

/**
 * AI ジョブの確認ダイアログを処理する。
 *
 * PLaMo はローカル推論なので外部送信の同意は出ない。既訳を上書きするときの確認だけが出る。
 * 送信同意（`AI翻訳の送信内容を確認`）が出たら、それは不具合なので失敗させる。
 */
async function acceptAiDialogs({ expectOverwrite = false, timeout = 60_000 } = {}) {
  let sawOverwrite = false;
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const dialog = await $('[role="dialog"]');
    if (!await dialog.isDisplayed().catch(() => false)) {
      if (sawOverwrite || !expectOverwrite) return sawOverwrite;
      await browser.pause(150);
      continue;
    }
    const dialogText = await dialog.getText();
    assert.equal(
      dialogText.includes(text.privacyTitle),
      false,
      `PLaMo で外部送信の同意ダイアログが出ています: ${dialogText}`,
    );
    if (dialogText.includes(text.overwriteTitle)) {
      sawOverwrite = true;
      await dialog.$(`button=${text.overwrite}`).click();
    } else {
      assert.fail(`想定外のダイアログです: ${dialogText}`);
    }
    await browser.waitUntil(async () => !await $('[role="dialog"]').isDisplayed().catch(() => false), {
      timeout: 15_000,
      interval: 100,
      timeoutMsg: "ダイアログが閉じませんでした。",
    });
  }
  return sawOverwrite;
}

/** 選択中のモデル・デバイスで足りないファイルがあれば、UI からダウンロードする。 */
async function downloadMissingFiles(timeout = 1_800_000) {
  const download = await findButton(text.downloadPrefix, { prefix: true });
  if (!download) return false;
  await download.click();
  const dialog = await $('[role="dialog"]');
  if (await dialog.isDisplayed().catch(() => false)) {
    await dialog.$(`button=${text.licenseAccept}`).click();
  }
  // 押した直後は再描画前でボタンがまだ残っている。進捗表示が出るのを待ってから
  // 完了を判定しないと、ダウンロード開始前に「終わった」と誤認してしまう。
  const progress = await $(`progress[aria-label="${text.progressAria}"]`);
  await progress.waitForDisplayed({ timeout: 120_000 });
  await browser.waitUntil(async () => (await findButton(text.downloadPrefix, { prefix: true })) === null, {
    timeout,
    interval: 500,
    timeoutMsg: "ダウンロードが完了しませんでした。",
  });
  // 失敗は設定画面のエラー行に出る。ここで拾わないと、後続の翻訳が理由不明で失敗する。
  const error = await $('.settings-field__hint--error[role="alert"]');
  if (await error.isDisplayed().catch(() => false)) {
    assert.fail(`PLaMo のダウンロードに失敗しました: ${await error.getText()}`);
  }
  return true;
}

/**
 * 接続テストを実行し、結果の文言を待つ。
 *
 * 直前の「保存しました」が残ったままだと結果を読み違えるため、文言が変わるまで待ってから
 * 判定する。ダウンロード直後は状態の再取得が終わっておらずボタンが無効なことがあるので、
 * 押せるようになるまで待つ。失敗時は実際に表示された文言とログ末尾をそのまま添える。
 */
async function testConnectionAndWait(timeout = 600_000) {
  const message = await $(".settings-message");
  const before = await message.isDisplayed().catch(() => false) ? (await message.getText()).trim() : "";
  const button = await findButton(text.testConnection);
  assert.ok(button, "接続テストのボタンがありません。");
  await browser.waitUntil(async () => button.isEnabled(), {
    timeout: 120_000,
    interval: 200,
    timeoutMsg: "接続テストのボタンが押せる状態になりませんでした。",
  });
  await button.click();
  await browser.waitUntil(async () => {
    const value = (await message.getText()).trim();
    return value !== "" && value !== before;
  }, { timeout, interval: 200, timeoutMsg: "接続テストの結果が表示されませんでした。" });
  const value = (await message.getText()).trim();
  assert.equal(value, text.connected, `接続テストに失敗しました: ${value}\n${await recentAppLog()}`);
}

/** ダウンロードのボタンを押してライセンス同意ダイアログを開く（同意はしない）。 */
async function openLicenseDialog() {
  const download = await findButton(text.downloadPrefix, { prefix: true });
  assert.ok(download, "ダウンロードボタンがありません。");
  await download.click();
  const dialog = await $('[role="dialog"]');
  await dialog.waitForDisplayed({ timeout: 60_000 });
  return dialog;
}

/**
 * ライセンス同意ダイアログをライトとダークの両方で撮る。
 *
 * テーマの切り替えは設定カテゴリを移動するため、パネルが作り直されてダイアログは閉じる。
 * 撮影のたびに開き直す（同意はしないのでダウンロードは始まらない）。
 */
async function captureLicenseDialogInBothThemes() {
  await captureSettingsInBothThemes("settings-plamo-license-dialog", {
    prepare: async () => {
      await openLicenseDialog();
    },
  });
}

async function waitForAiJobFinished(timeout = 600_000) {
  await browser.waitUntil(async () => !await $(".editor-ai-progress").isDisplayed().catch(() => false), {
    timeout,
    interval: 200,
    timeoutMsg: "AI翻訳の進捗表示が消えませんでした。",
  });
}

/**
 * 1 件だけの翻訳が終わるのを待つ。
 *
 * 数件の翻訳は 1 秒未満で終わることがあり、進捗バーの表示を待つと取りこぼす。ここでは
 * 進捗の有無ではなく、対象セルが空でない訳文になり、かつ進捗表示が消えたことを完了条件にする
 * （セルが見つからない場合の空文字を「変わった」と誤認しないため、空でないことも条件にする）。
 */
async function waitForSingleTranslation(key, before, timeout = 600_000) {
  await browser.waitUntil(async () => {
    if (await $(".editor-ai-progress").isDisplayed().catch(() => false)) return false;
    const current = await targetText(key);
    return current.trim() !== "" && current !== before;
  }, { timeout, interval: 200, timeoutMsg: `${key} の翻訳が完了しませんでした。` });
}

/**
 * 翻訳が終わって訳文が入るまで待つ（訳文が変わることは求めない）。
 *
 * 同じ項目を2回翻訳すると、モデルが同じ訳文を返すことがある。実行中の表示を両テーマで
 * 撮るために同じ項目を使い回すので、2回目は「適用された」ことだけを見る。
 */
async function waitForTranslationSettled(key, timeout = 600_000) {
  await browser.waitUntil(async () => {
    if (await $(".editor-ai-progress").isDisplayed().catch(() => false)) return false;
    return (await targetText(key)).trim() !== "";
  }, { timeout, interval: 200, timeoutMsg: `${key} の翻訳が終わりませんでした。` });
}

async function targetText(key) {
  return browser.execute((selector) => document.querySelector(selector)?.textContent ?? "", `[data-testid="translation-target-${key}"]`);
}

/**
 * 対象の行がAI翻訳で適用されるまで待つ。
 *
 * 訳文が同じでも「そのバックエンドで翻訳が走った」ことは、行の状態が `AI` に変わることで
 * 分かる。進捗バーの表示は数十ミリ秒で消えることがあり、開始直後に「進捗なし・訳文あり」で
 * 即座に真になってしまうため、状態の変化を完了条件にする。
 */
async function waitForAiOrigin(key, timeout = 600_000) {
  await browser.waitUntil(async () => {
    if (await $(".editor-ai-progress").isDisplayed().catch(() => false)) return false;
    return browser.execute((keySelector) => {
      const row = document.querySelector(keySelector)?.closest('[role="row"][data-entry-id]');
      return row?.querySelector(".editor-state")?.getAttribute("aria-label") === "AI";
    }, `[data-testid="translation-target-${key}"]`);
  }, { timeout, interval: 200, timeoutMsg: `${key} が AI 翻訳として適用されませんでした。` });
}

async function translatedCount() {
  const value = await browser.execute(() => {
    const element = document.querySelector(".editor-toolbar__progress span");
    return element?.textContent ?? "";
  });
  const match = /([\d,]+)\s*\/\s*([\d,]+)/.exec(value);
  return match ? Number(match[1].replaceAll(",", "")) : 0;
}

async function aiProgressCount() {
  const value = await browser.execute(() => document.querySelector(".editor-ai-progress span")?.textContent ?? "");
  const match = /([\d,]+)\s*\/\s*([\d,]+)/.exec(value);
  return match ? Number(match[1].replaceAll(",", "")) : 0;
}

async function openModTab(name) {
  if (await $(".settings-screen").isDisplayed().catch(() => false)) await closeSettings();
  const tabSelector = '[role="tablist"][aria-label="開いているmod"] [role="tab"]';
  const hasTab = await browser.execute((selector, expected) => (
    [...document.querySelectorAll(selector)].some((element) => element.textContent.includes(expected))
  ), tabSelector, name);
  if (!hasTab) {
    await $(".mod-tabs__all-button").click();
    await $(".mod-tabs__panel").waitForDisplayed({ timeout: 10_000 });
    await browser.execute((expected) => {
      const target = [...document.querySelectorAll(".mod-tabs__open-unit")]
        .find((element) => element.textContent.includes(expected));
      if (!target) throw new Error(`mod not listed: ${expected}`);
      target.click();
    }, name);
  } else {
    await browser.execute((selector, expected) => {
      const target = [...document.querySelectorAll(selector)]
        .find((element) => element.textContent.includes(expected));
      if (!target) throw new Error(`mod tab not found: ${expected}`);
      target.click();
    }, tabSelector, name);
  }
  await browser.waitUntil(async () => browser.execute((selector, expected) => (
    document.querySelector(selector)?.textContent.includes(expected) ?? false
  ), `${tabSelector}[aria-selected="true"]`, name), { timeout: 15_000, interval: 100 });
}

/** modタブの右クリックメニューから「このmodの未翻訳をAIで翻訳」を選ぶ。 */
async function translateWholeModFromTab(name) {
  const tabSelector = '[role="tablist"][aria-label="開いているmod"] [role="tab"]';
  await browser.execute((selector, expected) => {
    const target = [...document.querySelectorAll(selector)]
      .find((element) => element.textContent.includes(expected));
    if (!target) throw new Error(`mod tab not found: ${expected}`);
    target.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, view: window }));
  }, tabSelector, name);
  const action = await $('[role="menuitem"]*=このmodの未翻訳をAIで翻訳');
  await action.waitForDisplayed({ timeout: 10_000 });
  await action.click();
}

async function advanceReviewToEditor() {
  const editor = await $('[aria-label="翻訳エディター"]');
  const next = await $('[data-testid="review-next"]');
  for (let step = 0; step < 14; step += 1) {
    if (await editor.isExisting()) {
      await editor.waitForDisplayed({ timeout: 60_000 });
      return;
    }
    await next.waitForClickable({ timeout: 90_000 });
    const label = (await next.getText()).trim();
    if (label === "翻訳を始める") {
      await next.click();
      await editor.waitForDisplayed({ timeout: 120_000 });
      return;
    }
    assert.equal(label, "次へ", `Reviewを進められません: ${label}`);
    const heading = await $(".review-screen h1").getText();
    await next.click();
    await browser.waitUntil(async () => {
      const current = await $(".review-screen h1");
      return !await current.isExisting() || (await current.getText()) !== heading;
    }, { timeout: 60_000, interval: 100, timeoutMsg: "Reviewの次の手順へ進みませんでした。" });
  }
  assert.fail("Reviewの全手順を終えられませんでした。");
}

async function openProjectFromJar(jarPaths) {
  await $('[data-testid="home-open-jar"]').waitForClickable({ timeout: 60_000 });
  await browser.execute((paths) => {
    window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, jarPath: paths };
  }, jarPaths);
  await $('[data-testid="home-open-jar"]').click();
  await $(".review-screen h1").waitForDisplayed({ timeout: 240_000 });
  await advanceReviewToEditor();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 120_000 });
}

async function waitForExportCompletion() {
  await browser.waitUntil(async () => (
    await $(".export-completion h1").isDisplayed()
    || await $(".export-screen [role='alert']").isDisplayed()
  ), { timeout: 180_000, interval: 100, timeoutMsg: "書き出し結果が表示されませんでした。" });
  const error = await $(".export-screen [role='alert']");
  if (await error.isDisplayed()) assert.fail(`書き出しエラー: ${await error.getText()}`);
  await waitForText(".export-completion h1", text.exported, 15_000);
}

async function prepareResourcePackExport(outputPath) {
  await $('[data-testid="editor-export"]').click();
  await $(".export-screen").waitForDisplayed({ timeout: 30_000 });
  await $('[for="export-option-resourcePack"]').click();
  await browser.waitUntil(async () => await $("#export-option-resourcePack").isSelected(), {
    timeout: 10_000,
    interval: 100,
  });
  await $('[name="resource-pack-destination"][value="folder"]').click();
  await browser.execute((value) => {
    window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, exportPath: value };
  }, outputPath);
  const versionSelect = await $('[aria-label="Minecraft バージョン"]');
  if (!(await versionSelect.getText()).includes("1.20.1")) {
    await versionSelect.click();
    // 実 Mod の JAR から検出したバージョンは 1.20.1 とは限らない。候補にあれば優先し、
    // 無ければ先頭の候補（未選択以外）を使う。
    const preferred = await $('[role="option"][data-value="1.20.1"]');
    if (await preferred.isExisting()) {
      await preferred.click();
    } else {
      const values = await browser.execute(() => (
        [...document.querySelectorAll('[role="listbox"][aria-label="Minecraft バージョン"] [role="option"]')]
          .map((element) => element.getAttribute("data-value"))
          .filter((value) => value)
      ));
      assert.ok(values.length > 0, "Minecraft バージョンの候補がありません。");
      await $(`[role="option"][data-value="${values[0]}"]`).click();
    }
    await browser.waitUntil(async () => (await versionSelect.getText()).trim() !== "", {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "Minecraft バージョンを選べませんでした。",
    });
  }
}

async function hasPlamoNoticeLine() {
  return browser.execute(() => (
    [...document.querySelectorAll(".export-screen__hint")]
      .some((element) => element.textContent.includes("Built with PLaMo"))
  ));
}

/** PLaMo の通知行を画面内へ送る。通知は保存先やバージョンの下に出るため、既定では画面外にある。 */
async function scrollPlamoNoticeIntoView() {
  await browser.execute(() => {
    const notice = [...document.querySelectorAll(".export-screen__hint")]
      .find((element) => element.textContent.includes("Built with PLaMo"));
    if (!notice) throw new Error("PLaMo notice line not found");
    // スクロールするのはウィンドウではなく、書き出し画面を包む祖先要素。実際に
    // スクロールできる祖先を探して、通知行が中央に来るよう位置を合わせる。
    let scroller = notice.parentElement;
    while (scroller) {
      const style = getComputedStyle(scroller);
      if (/(auto|scroll)/.test(style.overflowY) && scroller.scrollHeight > scroller.clientHeight) break;
      scroller = scroller.parentElement;
    }
    const target = scroller ?? document.scrollingElement ?? document.documentElement;
    const bounds = notice.getBoundingClientRect();
    target.scrollTop += bounds.top - target.clientHeight / 2;
  });
  await browser.pause(300);
}

/**
 * 書き出しを開始し、途中で出る確認を片付ける。
 *
 * 既存の訳に書式の食い違いがあると「このまま書き出す」の確認が出る。ここで見たいのは
 * PLaMo の通知の有無なので、確認はそのまま進める。
 */
async function startExport() {
  await $('[data-testid="export-start"]').click();
  for (let step = 0; step < 4; step += 1) {
    const dialog = await $('[role="dialog"]');
    if (!await dialog.isDisplayed().catch(() => false)) break;
    const dialogText = await dialog.getText();
    const proceed = await dialog.$("button=このまま書き出す");
    if (await proceed.isDisplayed().catch(() => false)) {
      await proceed.click();
    } else {
      const overwrite = await dialog.$("button=上書き");
      if (await overwrite.isDisplayed().catch(() => false)) {
        await overwrite.click();
      } else {
        assert.fail(`想定外の書き出し確認です: ${dialogText}`);
      }
    }
    await browser.waitUntil(async () => (
      !await $('[role="dialog"]').isDisplayed().catch(() => false)
      || await $(".export-completion h1").isDisplayed()
    ), { timeout: 60_000, interval: 100, timeoutMsg: "書き出しの確認が閉じませんでした。" });
  }
  await waitForExportCompletion();
}

describePlamo("PLaMo ローカル翻訳の実機確認", function () {
  if (!enabled) {
    it("YAKUCRAFT_E2E_PLAMO=1 のときだけ実行する", () => {});
    return;
  }

  let resourcePackPath = "";
  let plainResourcePackPath = "";

  before(async function () {
    this.timeout(180_000);
    assert.ok(dataDirectory, "YAKUCRAFT_DATA_DIR がありません。");
    assert.ok(modJarPath, "PLaMo 検証用の Mod JAR パスがありません。");
    assert.ok(existsSync(modJarPath), `Mod JAR がありません: ${modJarPath}`);
    assert.ok(existsSync(createJarPath), `Create の JAR がありません: ${createJarPath}`);
    assert.ok(fixtureJarPath && existsSync(fixtureJarPath), `fixture JAR がありません: ${fixtureJarPath}`);
    const temporaryRoot = path.dirname(dataDirectory);
    resourcePackPath = path.join(temporaryRoot, "plamo-resource-pack.zip");
    plainResourcePackPath = path.join(temporaryRoot, "plain-resource-pack.zip");
    await $('[data-testid="home-open-jar"]').waitForClickable({ timeout: 60_000 });
  });

  it("設定のPLaMoパネルを表示し、認証情報系の欄を出さない", async function () {
    this.timeout(240_000);
    await openSettings(text.aiCategory);
    await selectProvider("plamo", "PLaMo");

    await $('[role="combobox"][aria-label="モデル"]').waitForDisplayed({ timeout: 30_000 });
    await $('[role="combobox"][aria-label="デバイス"]').waitForDisplayed({ timeout: 10_000 });

    const fields = await browser.execute(() => ({
      apiKey: Boolean(document.querySelector('.settings-fields input[type="password"]')),
      url: Boolean(document.querySelector('.settings-fields input[type="url"]')),
      modelText: Boolean(document.querySelector("#ai-model")),
      attribution: [...document.querySelectorAll(".settings-field__hint")]
        .some((element) => element.textContent.trim() === "Built with PLaMo"),
    }));
    assert.equal(fields.apiKey, false, "PLaMo に APIキー欄が表示されています。");
    assert.equal(fields.url, false, "PLaMo に接続先URL欄が表示されています。");
    assert.equal(fields.modelText, false, "PLaMo に自由入力のモデル欄が表示されています。");
    assert.equal(fields.attribution, true, "Built with PLaMo の表示がありません。");

    // 詳細設定のうち、PLaMo が使わない欄（送信件数・追加指示）は出さない。
    await clickButton("詳細設定");
    const advanced = await browser.execute(() => ({
      batchSize: Boolean(document.querySelector("#ai-batch-size")),
      customPrompt: [...document.querySelectorAll("textarea")]
        .some((element) => element.closest(".settings-field")?.textContent.includes("追加指示")),
      concurrency: Boolean(document.querySelector('[role="combobox"][aria-label="並列数"]')),
    }));
    assert.equal(advanced.batchSize, false, "PLaMo に送信件数の欄が表示されています。");
    assert.equal(advanced.customPrompt, false, "PLaMo に追加指示の欄が表示されています。");
    assert.equal(advanced.concurrency, true, "PLaMo に並列数の欄がありません。");
    await clickButton("詳細設定");

    await selectOption("モデル", "plamo-3-translate-0.8b", "0.8B");
    await selectOption("デバイス", "gpu", "GPU");
    await captureSettingsInBothThemes("settings-plamo-idle");
  });

  it("翻訳先の選択肢でPLaMo非対応言語を選べなくする", async function () {
    this.timeout(180_000);
    await openSettings(text.languageCategory);
    // テーマ切り替えはカテゴリ移動を伴い、パネルごと作り直される。要素を取り直せるよう
    // 毎回セレクタから引く（掴んだ参照は再描画で無効になる）。
    const picker = () => $('[role="combobox"][aria-label="翻訳先の言語"]');
    await (await picker()).waitForDisplayed({ timeout: 20_000 });
    await browser.waitUntil(async () => await (await picker()).isEnabled(), {
      timeout: 30_000,
      interval: 100,
      timeoutMsg: "PLaMo の対応言語を取得できず、翻訳先が選べないままです。",
    });

    const optionState = async (code) => browser.execute((target) => {
      const option = [...document.querySelectorAll('[role="option"]')]
        .find((element) => element.querySelector("code")?.textContent === target);
      if (!option) return null;
      return {
        disabled: option.getAttribute("aria-disabled") === "true",
        reason: option.querySelector(".target-language-picker__unavailable")?.textContent ?? "",
      };
    }, code);
    const clickOption = async (code) => browser.execute((target) => {
      const option = [...document.querySelectorAll('[role="option"]')]
        .find((element) => element.querySelector("code")?.textContent === target);
      option?.click();
    }, code);
    const pickerValue = () => browser.execute(() => (
      document.querySelector('[role="combobox"][aria-label="翻訳先の言語"]')?.value ?? ""
    ));
    // 一覧は先頭から並ぶため、目的の言語を検索で絞ってから撮る（そうしないと
    // 非対応と対応の画像が同じ見た目になる）。
    const filterPicker = async (query) => {
      const input = await picker();
      await input.click();
      await browser.keys(["Control", "a"]);
      await browser.keys(query);
      await browser.waitUntil(async () => (await input.getValue()) === query, {
        timeout: 10_000,
        interval: 100,
        timeoutMsg: `翻訳先を「${query}」で絞れませんでした。`,
      });
    };

    await (await picker()).click();
    await browser.waitUntil(async () => (await optionState("pt_br")) !== null, {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "翻訳先の一覧に pt_br がありません。",
    });
    const portuguese = await optionState("pt_br");
    assert.equal(portuguese.disabled, true, "pt_br が PLaMo で選べてしまいます。");
    assert.match(portuguese.reason, /PLaMo/, "非対応の理由が表示されていません。");
    const turkish = await optionState("tr_tr");
    assert.equal(turkish.disabled, true, "tr_tr が PLaMo で選べてしまいます。");
    await captureSettingsInBothThemes("settings-target-language-disabled", {
      category: text.languageCategory,
      prepare: async () => {
        await filterPicker("Portugu");
        await browser.waitUntil(async () => (await optionState("pt_br")) !== null, {
          timeout: 10_000,
          interval: 100,
          timeoutMsg: "翻訳先の一覧が開きませんでした。",
        });
      },
    });

    // 撮影の後始末（テーマを戻す）でパネルが作り直され、一覧は閉じている。開き直して続ける。
    // Escape はフォーカスが入力欄の外にあると設定画面ごと閉じるため使わない。
    await (await picker()).click();
    await browser.waitUntil(async () => (await optionState("pt_br")) !== null, {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "翻訳先の一覧が開きませんでした。",
    });

    // 非対応言語は押しても選択が変わらない。一覧を開いている間は入力欄が検索語（空）を
    // 見せるため、選択状態は候補の aria-selected で確かめる。
    await clickOption("pt_br");
    await browser.pause(300);
    assert.equal(
      await browser.execute(() => {
        const option = [...document.querySelectorAll('[role="option"]')]
          .find((element) => element.querySelector("code")?.textContent === "ja_jp");
        return option?.getAttribute("aria-selected") === "true";
      }),
      true,
      "非対応言語を選べてしまいました。",
    );

    const korean = await optionState("ko_kr");
    assert.equal(korean.disabled, false, "ko_kr が PLaMo で選べません。");
    await clickOption("ko_kr");
    await browser.waitUntil(async () => /ko_kr/.test(await pickerValue()), {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "対応言語 ko_kr を選べませんでした。",
    });
    await captureSettingsInBothThemes("settings-target-language-supported", {
      category: text.languageCategory,
      prepare: async () => {
        await filterPicker("한국");
        await browser.waitUntil(async () => (await optionState("ko_kr")) !== null, {
          timeout: 10_000,
          interval: 100,
          timeoutMsg: "翻訳先の一覧が開きませんでした。",
        });
      },
    });

    // 翻訳対象のプロジェクトは ja_jp で作るため、翻訳先を戻す。
    await (await picker()).click();
    await clickOption("ja_jp");
    await browser.waitUntil(async () => /ja_jp/.test(await pickerValue()), { timeout: 10_000, interval: 100 });
  });

  it("ライセンス同意からダウンロードし、中断と再開を経て完了する", async function () {
    this.timeout(2_400_000);
    await openSettings(text.aiCategory);

    if (seeded) {
      const downloaded = await browser.execute(() => (
        document.querySelector('[role="combobox"][aria-label="モデル"]')?.textContent.includes("ダウンロード済み") ?? false
      ));
      if (downloaded) {
        console.log("シード済みのため、UIからの実ダウンロード確認は省略します。");
        await captureSettingsInBothThemes("settings-plamo-downloaded");
        return;
      }
    }

    // ライセンス未同意の状態から始める。同意なしにダウンロードが始まる回帰を検出できるよう、
    // ダイアログの表示と同意操作を必須にする（「出たら押す」では素通りしてしまう）。
    const dialog = await openLicenseDialog();
    assert.match(await dialog.getText(), /Preferred Networks/, "ライセンス本文の要約がありません。");

    // 同意する前はダウンロードが始まらないこと（進捗も .part も無いこと）を確かめる。
    await browser.pause(1_000);
    const progressSelector = `progress[aria-label="${text.progressAria}"]`;
    assert.equal(
      await $(progressSelector).isDisplayed().catch(() => false),
      false,
      "ライセンスに同意する前にダウンロードが始まっています。",
    );
    assert.deepEqual(await plamoPartFiles(), [], "ライセンスに同意する前に .part が作られています。");

    await captureLicenseDialogInBothThemes();

    // 撮影のために開き直したダイアログで、改めて同意してから取得を始める。
    const accepted = await openLicenseDialog();
    await accepted.$(`button=${text.licenseAccept}`).click();
    await $(progressSelector).waitForDisplayed({ timeout: 180_000 });
    await captureSettingsInBothThemes("settings-plamo-downloading");

    // 途中で中断し、`.part` が残ることを確かめてから再開する。
    await browser.waitUntil(async () => {
      const value = await $(progressSelector).getAttribute("value");
      return value !== null && Number(value) > 0;
    }, { timeout: 240_000, interval: 200, timeoutMsg: "ダウンロードの進捗が動きませんでした。" });
    await clickButton(text.cancel);
    await browser.waitUntil(async () => !await $(progressSelector).isDisplayed().catch(() => false), {
      timeout: 120_000,
      interval: 200,
      timeoutMsg: "中断後もダウンロード表示が残っています。",
    });
    await browser.waitUntil(async () => (await plamoPartFiles()).length > 0, {
      timeout: 30_000,
      interval: 200,
      timeoutMsg: "中断したダウンロードの .part が残っていません（レジュームできません）。",
    });
    await captureSettingsInBothThemes("settings-plamo-cancelled");

    // 中断コマンドは取り消しを要求してすぐ返るため、状態の再取得で表示が戻るまで待つ。
    await browser.waitUntil(async () => (await findButton(text.downloadPrefix, { prefix: true })) !== null, {
      timeout: 60_000,
      interval: 500,
      timeoutMsg: "中断後にダウンロードボタンが戻りませんでした。",
    });
    await (await findButton(text.downloadPrefix, { prefix: true })).click();
    await browser.waitUntil(async () => (await findButton(text.delete)) !== null, {
      timeout: 1_800_000,
      interval: 500,
      timeoutMsg: "ダウンロードが完了しませんでした。",
    });

    // 完了後は残り容量が 0 になり、ダウンロードのボタンが消える。
    assert.equal(await findButton(text.downloadPrefix, { prefix: true }), null, "完了後もダウンロードボタンが残っています。");
    assert.match(
      await $('[role="combobox"][aria-label="モデル"]').getText(),
      /ダウンロード済み/,
      "モデルがダウンロード済みとして表示されていません。",
    );
    const modelPath = path.join(dataDirectory, "plamo", "models", PLAMO_MODEL_FILE);
    assert.ok(existsSync(modelPath), `モデルが保存されていません: ${modelPath}`);
    assert.equal((await stat(modelPath)).size, PLAMO_MODEL_BYTES, "モデルのサイズが期待と違います。");
    assert.deepEqual(await plamoPartFiles(), [], "完了後も .part が残っています。");

    await captureSettingsInBothThemes("settings-plamo-downloaded");
  });

  it("保存して接続テストに成功する", async function () {
    this.timeout(600_000);
    await openSettings(text.aiCategory);
    await clickButton(text.save);
    await waitForText(".settings-message", text.saved, 30_000);
    await testConnectionAndWait();
    await captureSettingsInBothThemes("settings-plamo-configured");
  });

  it("ダウンロードに失敗すると、パネルにエラーを表示する", async function () {
    this.timeout(900_000);
    // 2B モデルを選び、書き込みを拒否したディレクトリへ取得させる。実ネットワークの
    // 失敗を待つのではなく、書き込み段階で確実に失敗させる（容量不足と同じ経路）。
    await openSettings(text.aiCategory);
    await selectOption("モデル", "plamo-3-translate-2b", "2B");
    await clickButton(text.save);
    await waitForText(".settings-message", text.saved, 30_000);

    const modelsDirectory = path.join(dataDirectory, "plamo", "models");
    // パネルのエラーは画面ローカルの状態で、テーマ切り替え（カテゴリ移動）で消える。
    // 撮影のたびに同じ失敗を起こし直す。
    const triggerFailure = async () => {
      const download = await findButton(text.downloadPrefix, { prefix: true });
      assert.ok(download, "ダウンロードボタンがありません。");
      await download.click();
      const dialog = await $('[role="dialog"]');
      if (await dialog.isDisplayed().catch(() => false)) {
        await dialog.$(`button=${text.licenseAccept}`).click();
      }
      const error = await $('.settings-field__hint--error[role="alert"]');
      await error.waitForDisplayed({ timeout: 600_000 });
      assert.match(await error.getText(), /書き込めませんでした/, "書き込み失敗の説明が出ていません。");
    };

    await denyWrite(modelsDirectory);
    try {
      await triggerFailure();
      await captureSettingsInBothThemes("settings-plamo-error", { prepare: triggerFailure });
    } finally {
      await allowWrite(modelsDirectory);
    }

    // 後続の検証に影響しないよう、0.8B に戻しておく。
    await selectOption("モデル", "plamo-3-translate-0.8b", "0.8B");
    await clickButton(text.save);
    await waitForText(".settings-message", text.saved, 30_000);
  });

  it("実Modを開き、%s と § を含む項目を翻訳しても書式コードを保つ", async function () {
    this.timeout(1_200_000);
    await closeSettings();
    await openProjectFromJar([modJarPath, createJarPath]);

    await openModTab("Farmer's Delight");
    await searchEditor(PERCENT_KEY);
    const percentBefore = await targetText(PERCENT_KEY);
    assert.match(percentBefore, /%s/, "前提となる %s 付きの既訳がありません。");
    await $(`[data-testid="translation-target-${PERCENT_KEY}"]`).click();
    assert.equal(await selectedCount(), 1, "行を選択できませんでした。");
    await captureScreenInBothThemes("editor-plamo-selection");

    // 実行中の表示は一瞬で消えるため、テーマごとに翻訳をやり直して撮る。
    // 同じ項目を2回翻訳すると同じ訳文が返ることがあるので、2回目は適用だけを待つ。
    for (const [themeName, fileName] of [["light", "editor-plamo-running"], ["dark", "editor-plamo-running-dark"]]) {
      await setTheme(themeName);
      await closeSettings();
      await $(`[data-testid="translation-target-${PERCENT_KEY}"]`).click();
      await translateSelection();
      await acceptAiDialogs({ expectOverwrite: true });
      await capture(fileName, 0);
      await waitForTranslationSettled(PERCENT_KEY);
    }
    await setTheme("light");
    await closeSettings();

    const percentAfter = await targetText(PERCENT_KEY);
    assert.notEqual(percentAfter, percentBefore, "%s を含む項目が翻訳されていません。");
    assert.match(percentAfter, /%s/, `%s が失われました: ${percentAfter}`);

    await openModTab("Create");
    await searchEditor(SECTION_KEY);
    const sectionBefore = await targetText(SECTION_KEY);
    assert.match(sectionBefore, /§7/, "前提となる § コード付きの既訳がありません。");
    await $(`[data-testid="translation-target-${SECTION_KEY}"]`).click();
    await translateSelection();
    await acceptAiDialogs({ expectOverwrite: true });
    await waitForSingleTranslation(SECTION_KEY, sectionBefore);
    const sectionAfter = await targetText(SECTION_KEY);
    assert.notEqual(sectionAfter, sectionBefore, "§ を含む項目が翻訳されていません。");
    assert.match(sectionAfter, /§7/, `§ コードが失われました: ${sectionAfter}`);
    // コードの後ろの説明文は訳される。コードだけが残って本文が消えていないことを確かめる。
    assert.match(sectionAfter, /§7\s*\S+/, `§ コードの後続テキストが失われました: ${sectionAfter}`);
    await captureScreenInBothThemes("editor-plamo-section-codes");
  });

  it("デバイスを切り替えて保存すると、サーバが新しいバックエンドで起動し直す", async function () {
    this.timeout(1_800_000);
    // 直前の検証が絞り込みを残していても、既訳の項目を検索で引けるようにする。
    await selectFilter("すべて");

    // GPU で起動し、Vulkan ビルドのサーバが動くことを確かめてから翻訳する。
    await openSettings(text.aiCategory);
    await selectOption("デバイス", "gpu", "GPU");
    await clickButton(text.save);
    await waitForText(".settings-message", text.saved, 30_000);
    await testConnectionAndWait();
    await waitForRunningBackend("gpu");
    await closeSettings();
    await openModTab("Create");
    await searchEditor(GPU_KEY);
    const gpuBefore = await targetText(GPU_KEY);
    await $(`[data-testid="translation-target-${GPU_KEY}"]`).click();
    await translateSelection();
    await acceptAiDialogs({ expectOverwrite: true });
    await waitForSingleTranslation(GPU_KEY, gpuBefore);
    const gpuText = await targetText(GPU_KEY);
    assert.match(gpuText, /%s/, `GPU の訳で %s が失われました: ${gpuText}`);

    // CPU へ切り替えると同じモデルでも別ランタイム・別バックエンドで起動し直す。
    // CPU 用ランタイムがまだ無ければ、ここで UI から取得する（シード済みなら不要）。
    await openSettings(text.aiCategory);
    await selectOption("デバイス", "cpu", "CPU");
    await captureSettingsInBothThemes("settings-plamo-cpu", {
      prepare: async () => {
        await selectOption("デバイス", "cpu", "CPU");
      },
    });
    await clickButton(text.save);
    await waitForText(".settings-message", text.saved, 30_000);
    await downloadMissingFiles();
    // 接続テストでサーバを起こし、CPU ビルドに切り替わったことを確かめてから翻訳する。
    await testConnectionAndWait();
    await waitForRunningBackend("cpu");
    await closeSettings();
    await openModTab("Create");
    await searchEditor(CPU_KEY);
    await $(`[data-testid="translation-target-${CPU_KEY}"]`).click();
    await translateSelection();
    await acceptAiDialogs({ expectOverwrite: true });
    await waitForAiOrigin(CPU_KEY);
    const cpuText = await targetText(CPU_KEY);
    assert.match(cpuText, /%s/, `CPU の訳で %s が失われました: ${cpuText}`);
    await captureScreenInBothThemes("editor-plamo-cpu-translated");

    // 後続の書き出しは GPU に戻してから行う。
    await openSettings(text.aiCategory);
    await selectOption("デバイス", "gpu", "GPU");
    await clickButton(text.save);
    await waitForText(".settings-message", text.saved, 30_000);
    await closeSettings();
  });

  it("未翻訳をまとめて翻訳し、進捗と完了を他プロバイダーと同じように示す", async function () {
    this.timeout(1_200_000);
    await openModTab("Farmer's Delight");
    await clearSearch();
    const before = await translatedCount();
    await translateWholeModFromTab("Farmer's Delight");
    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed({ timeout: 60_000 });
    const dialogText = await dialog.getText();
    assert.equal(dialogText.includes(text.privacyTitle), false, `外部送信の同意が出ています: ${dialogText}`);
    assert.match(dialogText, /このPCの外へ送信されません/, "ローカル実行であることの説明がありません。");
    await dialog.$(`button=${text.translate}`).click();

    await $(".editor-ai-progress").waitForDisplayed({ timeout: 120_000 });
    await browser.waitUntil(async () => (await aiProgressCount()) > 0, {
      timeout: 300_000,
      interval: 200,
      timeoutMsg: "AI翻訳の進捗が進みませんでした。",
    });
    await waitForAiJobFinished();
    const after = await translatedCount();
    assert.ok(after > before, `翻訳済み件数が増えていません (${before} -> ${after})`);
    await waitForText('[data-testid="editor-save-status"]', "保存済み", 60_000);
  });

  it("実行中のキャンセルで、適用済みの訳を残す", async function () {
    this.timeout(1_200_000);
    // 直前のテストの検索語が残っていると、行数が絞り込み後の値になってしまう。
    await clearSearch();
    // Create は未翻訳が 250 件以上あり、途中でキャンセルできるだけの時間がある。
    await openModTab("Create");
    await clearSearch();
    // 未翻訳だけに絞ると、適用済みの件数がそのまま行数の減少として読める。
    await selectFilter("未翻訳");
    const before = await visibleRowCount();
    assert.ok(before > 5, `キャンセルを試すには未翻訳が少なすぎます: ${before}`);
    await translateWholeModFromTab("Create");
    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed({ timeout: 60_000 });
    await dialog.$(`button=${text.translate}`).click();

    await $(".editor-ai-progress").waitForDisplayed({ timeout: 120_000 });
    // 進捗の件数はバッチの応答より先に進むことがあるため、実際に1バッチ適用された
    // （未翻訳の行が減った）ことを確認してからキャンセルする。
    await browser.waitUntil(async () => (await visibleRowCount()) < before, {
      timeout: 300_000,
      interval: 200,
      timeoutMsg: "キャンセル前に1バッチも適用されませんでした。",
    });
    const applied = await visibleRowCount();
    await clickButton(text.cancel, { scope: ".editor-ai-progress button" });
    await $(".editor-ai-notice").waitForDisplayed({ timeout: 180_000 });
    assert.match(await $(".editor-ai-notice").getText(), /キャンセル/, "キャンセルの通知文言が違います。");
    await captureScreenInBothThemes("editor-plamo-cancelled");

    // 適用済みの訳は残る。未翻訳フィルターの行数が減ったままであることで確かめる。
    await browser.pause(1_000);
    const kept = await visibleRowCount();
    assert.ok(
      kept <= applied && kept < before,
      `キャンセル後に適用済みの訳が失われました (未翻訳 ${before} → 適用後 ${applied} → キャンセル後 ${kept})`,
    );
    await waitForText('[data-testid="editor-save-status"]', "保存済み", 60_000);
    await selectFilter("すべて");
  });

  it("8B と CPU の組み合わせでだけ所要時間の注意を出す", async function () {
    this.timeout(240_000);
    await openSettings(text.aiCategory);
    const hintShown = () => browser.execute(() => (
      [...document.querySelectorAll(".settings-field__hint")]
        .some((element) => element.textContent.includes("CPU では時間がかかります"))
    ));
    await selectOption("モデル", "plamo-3-translate-8b", "8B");
    await selectOption("デバイス", "cpu", "CPU");
    await browser.waitUntil(hintShown, { timeout: 10_000, interval: 100, timeoutMsg: "8B + CPU の注意が表示されません。" });
    await captureSettingsInBothThemes("settings-plamo-cpu-8b-hint", {
      prepare: async () => {
        await selectOption("モデル", "plamo-3-translate-8b", "8B");
        await selectOption("デバイス", "cpu", "CPU");
        await browser.waitUntil(hintShown, { timeout: 10_000, interval: 100, timeoutMsg: "8B + CPU の注意が表示されません。" });
      },
    });
    await selectOption("デバイス", "gpu", "GPU");
    await browser.waitUntil(async () => !(await hintShown()), {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "GPU に戻しても注意が残っています。",
    });
    await selectOption("モデル", "plamo-3-translate-0.8b", "0.8B");
    await clickButton(text.save);
    await waitForText(".settings-message", text.saved, 30_000);
    await closeSettings();
  });

  it("PLaMo の訳を含む書き出しにライセンス通知を同梱する", async function () {
    this.timeout(900_000);
    await prepareResourcePackExport(resourcePackPath);
    assert.equal(await hasPlamoNoticeLine(), true, "書き出し画面に PLaMo の通知行がありません。");
    await captureScreenInBothThemes("export-plamo-notice", scrollPlamoNoticeIntoView);
    await startExport();
    const entries = readZipEntries(await readFile(resourcePackPath));
    assert.equal(entries.has(NOTICE_FILE), true, `書き出した ZIP に ${NOTICE_FILE} がありません。`);
    const noticeText = entries.get(NOTICE_FILE).toString("utf8");
    assert.equal(noticeText.split("\n")[0], "Built with PLaMo", "通知の先頭行が規約の文言と違います。");
    assert.match(noticeText, /PLaMo Community License/, "ライセンス本文が同梱されていません。");
    await captureScreenInBothThemes("export-plamo-complete");
  });

  it("PLaMo を使っていないプロジェクトの書き出しには通知を付けない", async function () {
    this.timeout(900_000);
    await $('.app-brand[aria-label="ホームへ戻る"]').click();
    await $(".home-screen").waitForDisplayed({ timeout: 30_000 });
    await openProjectFromJar(fixtureJarPath);
    await prepareResourcePackExport(plainResourcePackPath);
    assert.equal(await hasPlamoNoticeLine(), false, "PLaMo 未使用のプロジェクトに通知行が出ています。");
    await startExport();
    const entries = readZipEntries(await readFile(plainResourcePackPath));
    assert.equal(entries.has(NOTICE_FILE), false, `PLaMo 未使用なのに ${NOTICE_FILE} が入っています。`);
  });

  it("アプリを終了すると llama-server が残らない", async function () {
    this.timeout(600_000);
    // 終了前に PLaMo のサーバを起動しておき、停止経路を通す。
    await openSettings(text.aiCategory);
    await testConnectionAndWait();
    await closeSettings();
    assert.ok(llamaServerProcesses().length > 0, "終了前の llama-server が見つかりません（検証が空振りします）。");

    // `exit_app` は autosave 後の終了と同じ経路（`RunEvent::Exit` → `flush_and_close_project`）を
    // 通り、そこで `shutdown_blocking` が llama-server を止める。ウィンドウを閉じると
    // WebDriver セッションごと切れて後続の確認ができないため、コマンドを直接呼ぶ。
    await browser.execute(() => window.__TAURI_INTERNALS__.invoke("exit_app")).catch(() => {});

    // セッションが切れるため、以降は WebDriver ではなくプロセス一覧を直接見る。
    const deadline = Date.now() + 240_000;
    let remaining = llamaServerProcesses();
    while (remaining.length > 0 && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 500));
      remaining = llamaServerProcesses();
    }
    assert.deepEqual(
      remaining,
      [],
      `アプリ終了後も llama-server が残っています: ${remaining.join(" | ")}\n${await recentAppLog()}`,
    );
  });
});
