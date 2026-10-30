import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { generateFeatureFixtures, readZipEntries } from "../scripts/generate-feature-fixtures.mjs";

const modpackTabName = "Modpack 固有（クエスト・KubeJS）";
const patchouliKey = "body_guide › entries/start.json › pages[0].text";
const patchouliCategoryKey = "body_guide › categories/basics.json › name";
const ftbKey = "quest.fixture › description";
const kubeKey = "item.fixture.widget";

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

async function setInputValue(input, value) {
  if (process.platform === "linux") {
    await browser.execute((element, nextValue) => {
      const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      if (!setValue) throw new Error("input value setter is unavailable");
      setValue.call(element, nextValue);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    }, input, value);
    return;
  }
  await input.setValue(value);
}

async function scanFolder(directoryPath) {
  await browser.execute((value) => {
    window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, directoryPath: value };
  }, directoryPath);
  await $('[data-testid="home-open-mods-folder"]').click();
  await $(".scan-screen").waitForDisplayed({ timeout: 60_000 });
  await browser.waitUntil(async () => (await $$(".scan-screen__row")).length > 0, {
    timeout: 60_000,
    interval: 100,
    timeoutMsg: "スキャン結果が表示されませんでした。",
  });
}

async function scanRowFor(name) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    for (const row of await $$(".scan-screen__row")) {
      if (await browser.execute((element, expected) => element.textContent.includes(expected), row, name)) return row;
    }
    await browser.pause(100);
  }
  const rows = [];
  for (const row of await $$(".scan-screen__row")) rows.push(await row.getText());
  const screenText = await $(".scan-screen").getText().catch(() => "スキャン画面がありません。");
  assert.fail(`スキャン結果に「${name}」がありません。\n${rows.join("\n")}\n${screenText}`);
}

async function translationTargetText(key) {
  const selector = `[data-testid="translation-target-${key}"]`;
  return browser.execute((targetSelector) => document.querySelector(targetSelector)?.textContent ?? "", selector);
}

async function selectScanRow(name, selected) {
  const row = await scanRowFor(name);
  const checkbox = await row.$('input[type="checkbox"]');
  assert.equal(await checkbox.isEnabled(), true, `${name} は選択できるはずです。`);
  if (await checkbox.isSelected() !== selected) await row.$("label").click();
  assert.equal(await checkbox.isSelected(), selected);
}

async function advanceReviewToEditor() {
  const next = await $('[data-testid="review-next"]');
  for (let step = 0; step < 14; step += 1) {
    await next.waitForClickable({ timeout: 60_000 });
    const label = (await next.getText()).trim();
    if (label === "翻訳を始める") {
      await next.click();
      await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
      return;
    }
    assert.equal(label, "次へ", `Reviewを進められません: ${label}`);
    const heading = await $(".review-screen h1").getText();
    await next.click();
    await browser.waitUntil(async () => {
      const currentHeading = await $(".review-screen h1");
      return !await currentHeading.isExisting() || (await currentHeading.getText()) !== heading;
    }, { timeout: 15_000, interval: 100, timeoutMsg: "Reviewの次の手順へ進みませんでした。" });
  }
  assert.fail("Reviewの全手順を終えられませんでした。");
}

async function clickModTab(name) {
  const tabSelector = '[role="tablist"][aria-label="開いているmod"] [role="tab"]';
  let matchingTab = false;
  for (const tab of await $$(tabSelector)) {
    if (await browser.execute((element, expected) => element.textContent.includes(expected), tab, name)) matchingTab = true;
  }
  let selector = tabSelector;
  if (!matchingTab) {
    if (!await $(".mod-tabs__all-button").isExisting()) {
      assert.ok((await $$(tabSelector).length) <= 1, `modタブ「${name}」が表示されていません。`);
      return;
    }
    await $(".mod-tabs__all-button").click();
    await browser.waitUntil(async () => (await $(".mod-tabs__all-button").getAttribute("aria-expanded")) === "true", {
      timeout: 2_000,
      interval: 50,
      timeoutMsg: "すべてのmodの一覧を開けませんでした。",
    });
    await $(".mod-tabs__panel").waitForDisplayed({ timeout: 10_000 });
    const search = await $(".mod-tabs__panel .mod-tabs__search");
    await search.clearValue();
    await browser.pause(300);
    const candidates = [];
    for (const item of await $$(".mod-tabs__open-unit")) {
      candidates.push(await browser.execute((element) => element.textContent, item));
    }
    assert.ok(candidates.some((text) => text.includes(name)),
      `mod一覧に「${name}」がありませんでした。検索=${await search.getValue()}、候補=${candidates.join(" | ")}、一覧=${(await $(".mod-tabs__panel").getText()).slice(0, 500)}`);
    selector = ".mod-tabs__open-unit";
  }
  await browser.execute((targetSelector, targetName) => {
    const target = [...document.querySelectorAll(targetSelector)]
      .find((element) => element.textContent.includes(targetName));
    if (!target) throw new Error(`mod tab not found: ${targetName}`);
    target.click();
  }, selector, name);
  await browser.waitUntil(async () => {
    const activeTab = await $(tabSelector + '[aria-selected="true"]');
    return browser.execute((element, expected) => element.textContent.includes(expected), activeTab, name);
  }, { timeout: 10_000, interval: 100 });
}

async function editTranslation(key, value) {
  const target = await $(`[data-testid="translation-target-${key}"]`);
  await target.waitForDisplayed({ timeout: 30_000 });
  await target.click();
  await browser.keys("Enter");
  const editor = await $(`textarea[aria-label="${key} の日本語訳"]`);
  await editor.waitForDisplayed({ timeout: 5_000 });
  await browser.waitUntil(() => editor.isFocused(), { timeout: 5_000, interval: 50 });
  if (process.platform === "linux") {
    // WebKitDriver under Xvfb drops Shift for punctuation such as &, @, braces, and colon.
    await browser.execute((input, nextValue) => {
      const setValue = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      if (!setValue) throw new Error("textarea value setter is unavailable");
      setValue.call(input, nextValue);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, editor, value);
    await browser.waitUntil(async () => await editor.getValue() === value && await editor.isFocused(), {
      timeout: 5_000,
      interval: 50,
      timeoutMsg: `${key} の編集欄へ入力できませんでした。`,
    });
    await browser.execute((input) => input.blur(), editor);
  } else {
    await browser.keys(["Control", "A"]);
    await browser.keys(value);
    await browser.keys("Enter");
  }
  try {
    await browser.waitUntil(async () => (await translationTargetText(key)) === value, {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: `${key} の編集内容が表に反映されませんでした。`,
    });
  } catch (error) {
    const targetText = await translationTargetText(key);
    const editorValue = await editor.getValue().catch(() => null);
    throw new Error(`${key} の編集内容が表に反映されませんでした。表示=${JSON.stringify(targetText)} 入力=${JSON.stringify(editorValue)}`, { cause: error });
  }
  await waitForText('[data-testid="editor-save-status"]', "保存中…", 10_000).catch(async () => {
    await $('[data-testid="editor-save-status"]').waitForDisplayed({ timeout: 5_000 });
  });
  await waitForText('[data-testid="editor-save-status"]', "保存済み", 15_000);
}

async function waitForPlaceholderWarning(key, expected) {
  const target = await $(`[data-testid="translation-target-${key}"]`);
  const entryId = await target.getAttribute("data-entry-id");
  const warning = $(`[role="row"][data-entry-id="${entryId}"] .editor-table__warning`);
  await browser.waitUntil(async () => (await warning.isExisting()) === expected, {
    timeout: 10_000,
    interval: 100,
    timeoutMsg: `${key} の置換文字列警告が ${expected ? "表示" : "解除"} されませんでした。`,
  });
}

async function waitForExportCompletion() {
  try {
    await browser.waitUntil(async () =>
      await $(".export-completion h1").isDisplayed()
      || await $(".export-screen [role='alert']").isDisplayed(),
    { timeout: 90_000, interval: 100, timeoutMsg: "書き出し結果が表示されませんでした。" });
  } catch (error) {
    const screenText = await browser.execute(() => document.querySelector(".export-screen")?.innerText ?? document.body.innerText);
    console.error(`書き出し画面の状態:\n${screenText}`);
    throw error;
  }
  const error = await $(".export-screen [role='alert']");
  if (await error.isDisplayed()) assert.fail(`書き出しエラー: ${await error.getText()}`);
  await waitForText(".export-completion h1", "書き出しました", 10_000);
}

async function returnToExport() {
  await $('button=別の形式で書き出す').click();
  await $(".export-screen").waitForDisplayed({ timeout: 30_000 });
}

async function chooseExportKind(kind) {
  await $(".export-options").waitForDisplayed({ timeout: 30_000 });
  const label = await $(`[for="export-option-${kind}"]`);
  if (!await label.isExisting()) {
    throw new Error(`書き出し形式 ${kind} がありません:\n${await $(".export-screen").getText()}`);
  }
  await label.click();
  await browser.waitUntil(async () => {
    const option = await $(`#export-option-${kind}`);
    return await option.isSelected();
  }, { timeout: 5_000, interval: 100 });
}

async function selectOnlyExportUnit(name) {
  await $(".export-unit-selector").$("button=全解除").click();
  for (const label of await $$(".export-unit-selector label")) {
    if ((await browser.execute((element) => element.textContent, label)).includes(name)) {
      const checkbox = await label.$('input[type="checkbox"]');
      await label.click();
      assert.equal(await checkbox.isSelected(), true);
      return;
    }
  }
  assert.fail(`書き出し対象のmod「${name}」が見つかりません。`);
}

async function selectProvider(value, label) {
  await $('[role="combobox"][aria-label="プロバイダー"]').click();
  await $(`[role="option"][data-value="${value}"]`).click();
  await browser.waitUntil(async () =>
    (await $('[role="combobox"][aria-label="プロバイダー"]').getText()).includes(label),
  { timeout: 15_000, interval: 100, timeoutMsg: `プロバイダー「${label}」を選べませんでした。` });
}

async function openSettings() {
  for (const button of await $("nav.app-header__actions").$$("button")) {
    if ((await button.getText()).trim() === "設定") {
      await button.click();
      await $(".settings-screen").waitForDisplayed({ timeout: 15_000 });
      return;
    }
  }
  assert.fail("設定画面を開くボタンがありません。");
}

function startMockProvider() {
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = chunks.length > 0 ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : null;
    requests.push({
      method: request.method,
      path: request.url,
      authorization: request.headers.authorization,
      body,
    });
    if (request.method === "GET" && request.url.endsWith("/models")) {
      if (request.url.startsWith("/delayed/")) {
        await new Promise((resolve) => setTimeout(resolve, 900));
      }
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ data: [{ id: request.url.startsWith("/delayed/") ? "mock-model" : "old-model" }] }));
      return;
    }
    if (request.method === "POST" && request.url.endsWith("/chat/completions")) {
      const userMessage = body.messages.find((message) => message.role === "user");
      const payload = JSON.parse(userMessage.content);
      const translations = payload.entries.map((entry) => ({ id: entry.id, translation: "AI local translation" }));
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({
        choices: [{ message: { content: JSON.stringify({ translations }) } }],
      }));
      return;
    }
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not found" }));
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") return reject(new Error("mock server port was unavailable"));
      resolve({ server, requests, url: `http://127.0.0.1:${address.port}` });
    });
  });
}

async function restoreFileDeploymentFor(pathFragment, expectRestored = true) {
  const normalizedPathFragment = pathFragment.replaceAll("\\", "/");
  const section = await $('[aria-labelledby="export-file-deployments-title"]');
  await section.waitForDisplayed({ timeout: 15_000 });
  for (const item of await section.$$("li")) {
    if ((await item.getText()).replaceAll("\\", "/").includes(normalizedPathFragment)) {
      await item.$("button").click();
      if (!expectRestored) {
        await browser.waitUntil(async () => await $("[aria-labelledby='export-file-deployments-title'] [role='alert']").isDisplayed(), {
          timeout: 20_000,
          interval: 100,
          timeoutMsg: `外部変更済みの配置ファイル ${pathFragment} の復元を拒否しませんでした。`,
        });
        return item;
      }
      try {
        await browser.waitUntil(async () => {
          return await browser.execute((fragment) =>
            ![...document.querySelectorAll('[aria-labelledby="export-file-deployments-title"] li')]
              .some((row) => row.textContent.replaceAll("\\", "/").includes(fragment)), normalizedPathFragment);
        }, { timeout: 20_000, interval: 100, timeoutMsg: `配置ファイル ${pathFragment} が復元後も一覧に残っています。` });
      } catch (error) {
        console.error(`配置ファイル復元後の状態:\n${await browser.execute(() => document.querySelector('[aria-labelledby="export-file-deployments-title"]')?.innerText ?? "一覧なし")}`);
        const alert = await $("[aria-labelledby='export-file-deployments-title'] [role='alert']");
        if (await alert.isDisplayed()) console.error(`復元エラー: ${await alert.getText()}`);
        throw error;
      }
      return item;
    }
  }
  assert.fail(`配置済みファイル「${pathFragment}」が見つかりません。`);
}

async function confirmExportDialogs(initialLabel) {
  await $('[role="dialog"]').waitForDisplayed({ timeout: 30_000 });
  let confirmLabel = initialLabel;
  while (true) {
    const previousText = await $('[role="dialog"]').getText();
    await $(`button=${confirmLabel}`).click();
    await browser.waitUntil(async () => (
      !await $('[role="dialog"]').isDisplayed().catch(() => false)
      || await $('[role="dialog"]').getText().catch(() => "") !== previousText
      || await $(".export-completion h1").isDisplayed()
      || await $(".export-screen [role='alert']").isDisplayed()
    ), { timeout: 90_000, interval: 100, timeoutMsg: "確認後の書き出し結果が表示されませんでした。" });
    const currentDialog = await $('[role="dialog"]');
    if (!await currentDialog.isDisplayed().catch(() => false)) return;
    const buttons = await currentDialog.$$("button");
    confirmLabel = await buttons[1].getText();
    assert.equal(confirmLabel, "上書き", "想定外の書き出し確認が表示されました。");
  }
}

async function directModpackExport() {
  await chooseExportKind("modpackFiles");
  await $('[name="modpack-files-destination"][value="instance"]').click();
  await $('[data-testid="export-start"]').click();
  await confirmExportDialogs("配置");
}

describe("全機能の動作確認", () => {
  it("Patchouli・FTB・KubeJS、出力と復元、AI設定を実アプリで通す", async function () {
    this.timeout(480_000);
    const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "yakucraft-full-e2e-"));
    const fixturesRoot = path.join(temporaryRoot, "fixtures");
    const fixtures = await generateFeatureFixtures(fixturesRoot);
    const resourcePackPath = path.join(temporaryRoot, "feature-pack.zip");
    const modpackZipPath = path.join(temporaryRoot, "modpack-files.zip");
    const modifiedJarDirectory = path.join(temporaryRoot, "modified-jars");
    const mock = await startMockProvider();
    await mkdir(modifiedJarDirectory, { recursive: true });

    try {
      await scanFolder(fixtures.singleInstance);
      const modpackRow = await scanRowFor("Modpack 固有（クエスト・KubeJS）");
      const modpackRowText = await browser.execute((element) => element.textContent, modpackRow);
      assert.match(modpackRowText, /3 件/);
      assert.match(modpackRowText, /クエスト 2 件 · KubeJS 1 件/);
      await scanRowFor("Fixture i18n Guide");
      await scanRowFor("Fixture Body Guide");

      for (const row of await $$(".scan-screen__row")) {
        const checkbox = await row.$('input[type="checkbox"]');
        if (await checkbox.isEnabled() && await checkbox.isSelected()) await row.$("label").click();
      }
      await selectScanRow("Fixture i18n Guide", true);
      await selectScanRow("Fixture Body Guide", true);
      await selectScanRow("Modpack 固有（クエスト・KubeJS）", true);
      const createProject = await $(".scan-screen__actions button");
      assert.match((await createProject.getText()).trim(), /^3 件でプロジェクトを作成$/);
      await createProject.click();
      await $(".review-screen h1").waitForDisplayed({ timeout: 90_000 });
      await advanceReviewToEditor();

      await clickModTab(modpackTabName);
      assert.equal(await $(".editor-table").getAttribute("aria-rowcount"), "4", "Modpack の3件が編集対象に出るはずです。");
      await $(`[data-testid="translation-target-${ftbKey}"]`).waitForDisplayed({ timeout: 30_000 });
      await $(`[data-testid="translation-target-${kubeKey}"]`).waitForDisplayed({ timeout: 30_000 });
      await clickModTab("Fixture i18n Guide");
      await $('[data-testid="translation-target-block.fixture.relic"]').waitForDisplayed({ timeout: 30_000 });
      await clickModTab("Fixture Body Guide");
      const bodyTargets = await browser.execute(() => [...document.querySelectorAll('[data-testid^="translation-target-"]')]
        .map((target) => target.getAttribute("data-testid")));
      assert.ok(bodyTargets.includes(`translation-target-${patchouliKey}`),
        `Patchouli の実際のキー: ${bodyTargets.join(" | ")}`);
      await $(`[data-testid="translation-target-${patchouliCategoryKey}"]`).waitForDisplayed({ timeout: 30_000 });
      await $(`[data-testid="translation-target-${patchouliKey}"]`).waitForDisplayed({ timeout: 30_000 });
      await clickModTab(modpackTabName);
      await editTranslation(ftbKey, "色指定とページ区切りを壊した説明 {image:fixture:map}");
      await waitForPlaceholderWarning(ftbKey, true);
      await clickModTab("Fixture Body Guide");
      await $(`[data-testid="translation-target-${patchouliKey}"]`).waitForDisplayed({ timeout: 30_000 });
      await editTranslation(patchouliKey, "置換文字列を壊した説明");
      await waitForPlaceholderWarning(patchouliKey, true);

      await $('.app-brand[aria-label="ホームへ戻る"]').click();
      await $(".home-screen").waitForDisplayed({ timeout: 15_000 });
      let recentProject;
      const recentProjectTexts = [];
      await browser.waitUntil(async () => {
        recentProjectTexts.length = 0;
        for (const item of await $$(".recent-projects__item")) {
          const itemText = await browser.execute((element) => element.textContent, item);
          recentProjectTexts.push(itemText);
          if (itemText.includes("single-instance")) {
            recentProject = item;
            return true;
          }
        }
        return false;
      }, {
        timeout: 30_000,
        interval: 100,
        timeoutMsg: `作成したプロジェクトが最近の一覧にありません。候補=${recentProjectTexts.join(" | ")}`,
      });
      assert.ok(recentProject, "作成したプロジェクトが最近の一覧にありません。");
      await recentProject.$(".recent-projects__open").click();
      await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
      await clickModTab("Fixture Body Guide");
      assert.equal(await translationTargetText(patchouliKey), "置換文字列を壊した説明");
      await waitForPlaceholderWarning(patchouliKey, true);
      await clickModTab(modpackTabName);
      assert.equal(await translationTargetText(ftbKey), "色指定とページ区切りを壊した説明 {image:fixture:map}");
      await waitForPlaceholderWarning(ftbKey, true);

      const modpackTab = await $('[role="tablist"][aria-label="開いているmod"] [role="tab"][aria-selected="true"]');
      await modpackTab.click({ button: "right" });
      const refreshAction = await $('[role="menuitem"]*=Modpack を再取り込み');
      await refreshAction.waitForDisplayed({ timeout: 5_000 });
      await refreshAction.click();
      await browser.waitUntil(async () => await $(`[data-testid="translation-target-${ftbKey}"]`).isDisplayed(), {
        timeout: 30_000,
        interval: 100,
      });
      assert.equal(await translationTargetText(ftbKey), "色指定とページ区切りを壊した説明 {image:fixture:map}");
      await waitForPlaceholderWarning(ftbKey, true);

      await editTranslation(ftbKey, "&a遺物を探す {@pagebreak} {image:fixture:map}");
      await waitForPlaceholderWarning(ftbKey, false);
      await clickModTab("Fixture Body Guide");
      await editTranslation(patchouliKey, "$(item) 古代の遺物を見つけます。");
      await waitForPlaceholderWarning(patchouliKey, false);
      await clickModTab(modpackTabName);

      await openSettings();
      if (process.env.YAKUCRAFT_E2E_XDG_OPEN === "1") {
        for (const button of await $('[aria-label="設定カテゴリ"]').$$("button")) {
          if ((await button.getText()).trim() === "一般") {
            await button.click();
            break;
          }
        }
        const autoOpenLabel = await $(
          '//label[.//span[normalize-space()="書き出し後にフォルダーを開く"]]',
        );
        const autoOpenCheckbox = await autoOpenLabel.$('input[type="checkbox"]');
        if (!(await autoOpenCheckbox.isSelected())) await autoOpenLabel.click();
        await browser.waitUntil(() => autoOpenCheckbox.isSelected(), {
          timeout: 5_000,
          interval: 50,
        });
      }
      for (const button of await $('[aria-label="設定カテゴリ"]').$$("button")) {
        if ((await button.getText()).trim() === "AI翻訳") await button.click();
      }
      await $('[role="combobox"][aria-label="プロバイダー"]').waitForDisplayed({ timeout: 10_000 });
      const presets = [
        ["chatgpt", "ChatGPT（サブスクリプション）", false, false, true],
        ["openai", "OpenAI", true, false, false],
        ["gemini", "Gemini", true, false, false],
        ["openrouter", "OpenRouter", true, false, false],
        ["deepseek", "DeepSeek", true, false, false],
        ["groq", "Groq", true, false, false],
        ["ollama", "Ollama", false, false, false],
        ["lm-studio", "LM Studio", false, false, false],
        ["anthropic", "Claude（Anthropic）", true, false, false],
        ["openai-compatible", "OpenAI 互換（カスタム）", true, true, false],
      ];
      for (const [provider, label, hasApiKey, hasUrl, hasChatGptModel] of presets) {
        await selectProvider(provider, label);
        const fields = await browser.execute(() => ({
          apiKey: Boolean(document.querySelector('.settings-fields input[type="password"]')),
          url: Boolean(document.querySelector('.settings-fields input[type="url"]')),
          chatGptModel: Boolean(document.querySelector('[role="combobox"][aria-label="モデル"]')),
          modelInput: [...document.querySelectorAll(".settings-fields label")]
            .some((item) => item.textContent.trim().startsWith("モデル") && item.querySelector("input:not([type]), input[type='text']")),
        }));
        assert.equal(fields.apiKey, hasApiKey, `${label} のAPIキー欄が正しくありません。`);
        assert.equal(fields.url, hasUrl, `${label} の接続先URL欄が正しくありません。`);
        assert.equal(fields.chatGptModel, hasChatGptModel, `${label} のChatGPTモデル欄が正しくありません。`);
        assert.equal(fields.modelInput, !hasChatGptModel && provider !== "deepl", `${label} のモデル入力欄が正しくありません。`);
        if (provider === "chatgpt") await $("button=ChatGPTでログイン").waitForDisplayed({ timeout: 5_000 });
      }

      const baseUrl = `${mock.url}/v1`;
      const delayedBaseUrl = `${mock.url}/delayed/v1`;
      await setInputValue(await $("input[type='url']"), baseUrl);
      await $("input[type='password']").setValue("e2e-local-key");
      let modelInput;
      for (const label of await $$(".settings-fields label")) {
        if ((await label.getText()).trim().startsWith("モデル")) modelInput = await label.$("input");
      }
      assert.ok(modelInput, "モデル入力欄がありません。");
      await modelInput.setValue("mock-model");
      await $('button=保存').click();
      await browser.waitUntil(async () => mock.requests.some((request) => request.path === "/v1/models"), {
        timeout: 20_000,
        interval: 100,
        timeoutMsg: "保存したキーでモデル一覧を取得しませんでした。",
      });
      await browser.waitUntil(async () => await browser.execute(() =>
        document.querySelector("#ai-model-options option")?.textContent === "old-model"), {
        timeout: 10_000,
        interval: 100,
      });
      assert.equal(await $("input[type='password']").getValue(), "", "保存済みのAPIキーは入力欄に戻りません。");
      assert.equal(mock.requests.find((request) => request.path === "/v1/models")?.authorization, "Bearer e2e-local-key");

      await setInputValue(await $("input[type='url']"), delayedBaseUrl);
      await browser.waitUntil(async () => mock.requests.some((request) => request.path === "/delayed/v1/models"), {
        timeout: 20_000,
        interval: 100,
        timeoutMsg: "保存済みAPIキーで変更後のURLへ再取得しませんでした。",
      });
      assert.equal(await browser.execute(() => document.querySelector("#ai-model-options option")?.textContent ?? null), null,
        "再取得中に古いモデル候補が残っています。");
      await browser.waitUntil(async () => await browser.execute(() =>
        document.querySelector("#ai-model-options option")?.textContent === "mock-model"), {
        timeout: 15_000,
        interval: 100,
      });
      await $("button=保存").click();
      await browser.waitUntil(async () => await $(".settings-message").getText() === "保存しました", {
        timeout: 10_000,
        interval: 100,
        timeoutMsg: "変更した接続先URLを保存できませんでした。",
      });

      await selectProvider("gemini", "Gemini");
      assert.equal(await browser.execute(() => [...document.querySelectorAll(".settings-field__hint")]
        .some((hint) => hint.textContent.trim() === "設定済み")), false, "カスタムのキーがGeminiへ共有されています。");
      await selectProvider("openai-compatible", "OpenAI 互換（カスタム）");
      assert.equal(await $("input[type='password']").getValue(), "");
      assert.equal(await browser.execute(() => [...document.querySelectorAll(".settings-field__hint")]
        .some((hint) => hint.textContent.trim() === "設定済み")), true);
      await $("button=戻る").click();
      await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 15_000 });

      await clickModTab(modpackTabName);
      const kubeTarget = await $(`[data-testid="translation-target-${kubeKey}"]`);
      await kubeTarget.click();
      const selectedToolbar = await $('[role="toolbar"][aria-label="選択中の項目"]');
      for (const button of await selectedToolbar.$$("button")) {
        if ((await button.getText()).trim() === "AIで翻訳") await button.click();
      }
      const privacyDialog = await $("[role='dialog']");
      if (await privacyDialog.isDisplayed()) await privacyDialog.$("button=続ける").click();
      await browser.waitUntil(async () => mock.requests.some((request) => request.path === "/delayed/v1/chat/completions"), {
        timeout: 60_000,
        interval: 100,
        timeoutMsg: "ローカルモックへのAI翻訳リクエストがありません。",
      });
      await browser.waitUntil(async () => (await translationTargetText(kubeKey)) === "AI local translation", {
        timeout: 60_000,
        interval: 100,
        timeoutMsg: "AI翻訳の結果が保存されませんでした。",
      });
      const chatRequest = mock.requests.find((request) => request.path === "/delayed/v1/chat/completions");
      assert.equal(chatRequest.authorization, "Bearer e2e-local-key");
      assert.equal(chatRequest.body.model, "mock-model");
      assert.equal(chatRequest.body.response_format.type, "json_schema");

      if (process.env.YAKUCRAFT_E2E_LINUX_EXTRAS === "1") {
        await kubeTarget.click();
        await browser.keys(["Control", "c"]);
        await kubeTarget.click();
        await browser.keys("Enter");
        const clipboardEditor = await $(`textarea[aria-label="${kubeKey} の日本語訳"]`);
        await clipboardEditor.waitForDisplayed({ timeout: 5_000 });
        await browser.waitUntil(() => clipboardEditor.isFocused(), { timeout: 5_000, interval: 50 });
        await browser.execute((input, value) => {
          const setValue = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
          if (!setValue) throw new Error("textarea value setter is unavailable");
          setValue.call(input, value);
          input.dispatchEvent(new Event("input", { bubbles: true }));
        }, clipboardEditor, "clipboard sentinel");
        assert.equal(await clipboardEditor.getValue(), "clipboard sentinel", "貼り付け前の編集値を変更できませんでした。");
        await browser.keys(["Control", "A"]);
        await browser.keys(["Control", "v"]);
        await browser.waitUntil(async () => await clipboardEditor.getValue() === "AI local translation", {
          timeout: 5_000,
          interval: 50,
          timeoutMsg: "コピーした翻訳を編集欄へ貼り付けできませんでした。",
        });
        await browser.keys("Enter");
        await browser.waitUntil(async () => (await translationTargetText(kubeKey)) === "AI local translation", {
          timeout: 10_000,
          interval: 100,
          timeoutMsg: "Linux WebKitの貼り付けが翻訳へ反映されませんでした。",
        });
      }

      await openSettings();
      for (const button of await $('[aria-label="設定カテゴリ"]').$$("button")) {
        if ((await button.getText()).trim() === "AI翻訳") await button.click();
      }
      await $("button=削除").click();
      await browser.waitUntil(async () => !await browser.execute(() => [...document.querySelectorAll(".settings-field__hint")]
        .some((hint) => hint.textContent.trim() === "設定済み")), { timeout: 10_000, interval: 100 });
      await $("button=戻る").click();
      await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 10_000 });

      await $('[data-testid="editor-export"]').click();
      await $(".export-screen").waitForDisplayed({ timeout: 30_000 });
      await chooseExportKind("resourcePack");
      await $('[name="resource-pack-destination"][value="folder"]').click();
      await browser.execute((value) => {
        window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, exportPath: value };
      }, resourcePackPath);
      const versionSelect = await $('[aria-label="Minecraft バージョン"]');
      if (!(await versionSelect.getText()).includes("1.20.1")) {
        await versionSelect.click();
        await $('[role="option"][data-value="1.20.1"]').click();
      }
      await $('[data-testid="export-start"]').click();
      await waitForExportCompletion();
      const packEntries = readZipEntries(await readFile(resourcePackPath));
      assert.equal(packEntries.has("pack.mcmeta"), true);
      const patchouliOutputPath = "assets/fixture/patchouli_books/body_guide/ja_jp/entries/start.json";
      const patchouliOutput = JSON.parse(packEntries.get(patchouliOutputPath).toString("utf8"));
      assert.equal(patchouliOutput.pages[0].text, "$(item) 古代の遺物を見つけます。");
      const kubeOutput = JSON.parse(packEntries.get("assets/fixture/lang/ja_jp.json").toString("utf8"));
      assert.equal(kubeOutput["item.fixture.widget"], "AI local translation");

      await returnToExport();
      await chooseExportKind("modifiedJar");
      await selectOnlyExportUnit("Fixture Body Guide");
      await browser.execute((value) => {
        window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, directoryPath: value };
      }, modifiedJarDirectory);
      await $('[data-testid="export-start"]').click();
      await waitForExportCompletion();
      const modifiedJar = (await readdir(modifiedJarDirectory)).find((name) => name.endsWith(".jar"));
      assert.ok(modifiedJar, "Modified JAR が出力されませんでした。");
      const modifiedEntries = readZipEntries(await readFile(path.join(modifiedJarDirectory, modifiedJar)));
      const modifiedPatchouli = JSON.parse(modifiedEntries.get(patchouliOutputPath).toString("utf8"));
      assert.equal(modifiedPatchouli.pages[0].text, "$(item) 古代の遺物を見つけます。");

      await returnToExport();
      await chooseExportKind("modpackFiles");
      await selectOnlyExportUnit(modpackTabName);
      await $('[name="modpack-files-destination"][value="folder"]').click();
      await browser.execute((value) => {
        window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, exportPath: value };
      }, modpackZipPath);
      await $('[data-testid="export-start"]').click();
      await waitForExportCompletion();
      const modpackEntries = readZipEntries(await readFile(modpackZipPath));
      const ftbOutputPath = "config/ftbquests/quests/lang/ja_jp.snbt";
      const ftbOutput = modpackEntries.get(ftbOutputPath).toString("utf8");
      assert.match(ftbOutput, /&a遺物を探す \{@pagebreak\} \{image:fixture:map\}/);
      const modpackKube = JSON.parse(modpackEntries.get("kubejs/assets/fixture/lang/ja_jp.json").toString("utf8"));
      assert.equal(modpackKube["item.fixture.widget"], "AI local translation");

      await returnToExport();
      await chooseExportKind("modpackFiles");
      await selectOnlyExportUnit(modpackTabName);
      await $('[name="modpack-files-destination"][value="instance"]').click();
      const symlinkTarget = path.join(temporaryRoot, "symlink-target.json");
      const kubeTargetPath = path.join(fixtures.singleInstance, "kubejs/assets/fixture/lang/ja_jp.json");
      let symlinkAvailable = false;
      await mkdir(path.dirname(kubeTargetPath), { recursive: true });
      await writeFile(symlinkTarget, "external target\n", "utf8");
      try {
        await symlink(symlinkTarget, kubeTargetPath, "file");
        symlinkAvailable = true;
      } catch (error) {
        console.log(`シンボリックリンク確認を省略しました: ${error.code ?? error.message}`);
      }
      if (symlinkAvailable) {
        await directModpackExport();
        await browser.waitUntil(async () => await $(".export-screen [role='alert']").isDisplayed(), {
          timeout: 30_000,
          interval: 100,
          timeoutMsg: "シンボリックリンクへの直接配置を拒否しませんでした。",
        });
        assert.equal(await readFile(symlinkTarget, "utf8"), "external target\n");
        await rm(kubeTargetPath, { force: true });
      }
      const existingFtbPath = path.join(fixtures.singleInstance, "config/ftbquests/quests/lang/ja_jp.snbt");
      const originalFtbBytes = await readFile(existingFtbPath);
      await $('[data-testid="export-start"]').click();
      await confirmExportDialogs("配置");
      await waitForExportCompletion();
      assert.equal(await readFile(kubeTargetPath, "utf8").then((value) => JSON.parse(value)["item.fixture.widget"]), "AI local translation");
      assert.match(await readFile(existingFtbPath, "utf8"), /&a遺物を探す/);

      await returnToExport();
      await restoreFileDeploymentFor("ja_jp.snbt");
      await restoreFileDeploymentFor("kubejs/assets/fixture/lang/ja_jp.json");
      assert.deepEqual(await readFile(existingFtbPath), originalFtbBytes, "既存のFTB訳が復元されるはずです。");
      assert.equal(await import("node:fs/promises").then(({ stat }) => stat(kubeTargetPath).then(() => true).catch(() => false)), false,
        "新規のKubeJS訳は復元後に削除されるはずです。");

      await directModpackExport();
      await waitForExportCompletion();
      const changedFtbBytes = Buffer.from("external edit\n", "utf8");
      await writeFile(existingFtbPath, changedFtbBytes);
      await returnToExport();
      await directModpackExport();
      await browser.waitUntil(async () => await $(".export-screen [role='alert']").isDisplayed(), {
        timeout: 30_000,
        interval: 100,
        timeoutMsg: "外部変更済みのファイルへの再配置を拒否しませんでした。",
      });
      assert.match(await $(".export-screen [role='alert']").getText(), /ja_jp\.snbt/);
      await restoreFileDeploymentFor("ja_jp.snbt", false);
      await browser.waitUntil(async () => await $(".export-replacements [role='alert']").isDisplayed(), {
        timeout: 20_000,
        interval: 100,
        timeoutMsg: "外部変更済みのファイルの復元を拒否しませんでした。",
      });
      assert.deepEqual(await readFile(existingFtbPath), changedFtbBytes);
      await writeFile(existingFtbPath, originalFtbBytes);

      await $('.app-brand[aria-label="ホームへ戻る"]').click();
      await $(".home-screen").waitForDisplayed({ timeout: 15_000 });
      await scanFolder(fixtures.splitInstance);
      const splitRow = await scanRowFor("Modpack 固有（クエスト・KubeJS）");
      const splitRowText = await browser.execute((element) => element.textContent, splitRow);
      assert.match(splitRowText, /1 件/);
      assert.match(splitRowText, /クエスト 1 件 · KubeJS 0 件/);
      await selectScanRow("Modpack 固有（クエスト・KubeJS）", true);
      await $(".scan-screen__actions button").click();
      await $(".review-screen h1").waitForDisplayed({ timeout: 60_000 });
      await advanceReviewToEditor();
      await clickModTab(modpackTabName);
      const splitKey = "quest.split › タイトル";
      await $(`[data-testid="translation-target-${splitKey}"]`).waitForDisplayed({ timeout: 30_000 });
      await editTranslation(splitKey, "&b分割されたクエスト {@pagebreak}");
      await $('[data-testid="editor-export"]').click();
      await $(".export-screen").waitForDisplayed({ timeout: 30_000 });
      await chooseExportKind("modpackFiles");
      await $('[name="modpack-files-destination"][value="folder"]').click();
      const splitZipPath = path.join(temporaryRoot, "split-modpack.zip");
      await browser.execute((value) => {
        window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, exportPath: value };
      }, splitZipPath);
      await $('[data-testid="export-start"]').click();
      await waitForExportCompletion();
      const splitEntries = readZipEntries(await readFile(splitZipPath));
      const splitOutputPath = "config/ftbquests/quests/lang/ja_jp/chapters/start.snbt";
      assert.match(splitEntries.get(splitOutputPath).toString("utf8"), /&b分割されたクエスト \{@pagebreak\}/);

      await $('.app-brand[aria-label="ホームへ戻る"]').click();
      await $(".home-screen").waitForDisplayed({ timeout: 15_000 });
      await scanFolder(fixtures.legacyInstance);
      const legacyRow = await scanRowFor("Modpack 固有（クエスト・KubeJS）");
      const legacyRowText = await browser.execute((element) => element.textContent, legacyRow);
      assert.match(legacyRowText, /クエスト旧形式/);
      assert.match(legacyRowText, /この形式のクエストは翻訳できません/);
      assert.equal(await legacyRow.$('input[type="checkbox"]').isEnabled(), false);
    } finally {
      mock.server.closeAllConnections?.();
      await new Promise((resolve) => mock.server.close(resolve));
      await rm(temporaryRoot, { recursive: true, force: true });
    }
  });
});
