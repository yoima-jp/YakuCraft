import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const editedTranslation = "E2E edited";
const draftTranslation = "E2E draft survives Ctrl+O";
const translationKey = "block.sample.machine";

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

async function readCellText(selector) {
  return browser.execute((targetSelector) => document.querySelector(targetSelector)?.textContent ?? "", selector);
}

describe("最初の翻訳マイルストーン", () => {
  it("fixtureを解析・編集・書き出し、再起動後にRecentから再開できる", async () => {
    const fixturePath = process.env.YAKUCRAFT_E2E_FIXTURE;
    const exportPath = process.env.YAKUCRAFT_E2E_EXPORT;
    assert.ok(fixturePath, "fixture JAR のパスがありません。");
    assert.ok(exportPath, "書き出し先のパスがありません。");

    await browser.execute((paths) => {
      window.__YAKUCRAFT_E2E__ = paths;
    }, { jarPath: fixturePath, exportPath });

    const chooseJar = await $('[data-testid="home-open-jar"]');
    await chooseJar.waitForClickable({ timeout: 30_000 });
    // ドラッグ&ドロップの購読は capability の core:event 権限に依存する。
    // 権限が外れると Home にエラーが出るだけで他の操作は通るため、ここで明示的に検出する。
    await browser.pause(500);
    assert.equal(await $(".home-screen [role=\"alert\"]").isExisting(), false, "Home にエラーが表示されています。");
    await chooseJar.click();

    const reviewSteps = [
      "Mod情報を確認",
      "翻訳元を確認",
      "既存の日本語訳を引き継ぎますか？",
    ];
    const reviewHeading = await $(".review-screen h1");
    const analysisError = await $(".home-screen [role=\"alert\"]");
    await browser.waitUntil(async () =>
      (await reviewHeading.isExisting()) || (await analysisError.isExisting()),
    {
      // 初回の解析にReview用asset index作成も続くため、画面遷移まで余裕を持って待つ。
      timeout: 90_000,
      interval: 100,
      timeoutMsg: "JAR解析後にReviewもエラーも表示されませんでした。",
    });
    if (await analysisError.isExisting()) {
      assert.fail("JARの解析に失敗しました: " + await analysisError.getText());
    }
    await $(".review-screen h1").waitForDisplayed({ timeout: 5_000 });
    for (const [index, heading] of reviewSteps.entries()) {
      await waitForText(".review-screen h1", heading, 60_000);
      await $('[data-testid="review-next"]').click();
      if (index === reviewSteps.length - 1) break;
    }

    const targetCellSelector = `[data-testid="translation-target-${translationKey}"]`;
    const targetCell = await $(targetCellSelector);
    await targetCell.waitForDisplayed({ timeout: 60_000 });
    const entryId = await targetCell.getAttribute("data-entry-id");
    await targetCell.click();
    await browser.keys("Enter");
    const editorSelector = `textarea[aria-label="${translationKey} の日本語訳"]`;
    const editor = await $(editorSelector);
    await editor.waitForDisplayed({ timeout: 5_000 });
    await browser.waitUntil(() => editor.isFocused(), { timeout: 5_000, interval: 50 });
    await browser.keys(["Control", "A"]);
    await browser.keys(draftTranslation);
    assert.equal(await editor.getValue(), draftTranslation, "編集中の日本語訳が入力欄に反映されていません。");

    // Ctrl+OでHomeへ移動した直後に選択をキャンセルし、Recentから同じProjectを読み直す。
    await browser.execute(() => { window.__YAKUCRAFT_E2E__.jarPath = null; });
    await browser.keys(["Control", "o"]);
    await $(".home-screen").waitForDisplayed({ timeout: 10_000 });
    const draftResumeProject = await $("button.recent-projects__open");
    await draftResumeProject.waitForClickable({ timeout: 30_000 });
    await draftResumeProject.click();
    await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 30_000 });
    await $(targetCellSelector).waitForDisplayed({ timeout: 30_000 });
    await browser.waitUntil(async () => (await readCellText(targetCellSelector)) === draftTranslation, {
      timeout: 10_000,
      interval: 100,
      timeoutMsg: "Ctrl+OでHomeへ移動しても、未確定の日本語訳が保存されませんでした。",
    });
    const resumedCell = await $(targetCellSelector);
    assert.equal(await readCellText(targetCellSelector), draftTranslation);

    await resumedCell.click();
    await browser.keys("Enter");
    await $(editorSelector).waitForDisplayed({ timeout: 5_000 });
    await browser.waitUntil(() => $(editorSelector).then((element) => element.isFocused()), { timeout: 5_000, interval: 50 });
    // 仮想行はReact再描画で要素が差し替わるため、フォーカス済みWebViewへキーを送る。
    await browser.keys(["Control", "A"]);
    await browser.keys(editedTranslation);
    await browser.keys("Enter");

    const saveStatus = await $('[data-testid="editor-save-status"]');
    await waitForText('[data-testid="editor-save-status"]', "保存中…", 10_000).catch(async () => {
      // 短時間で保存が完了した場合も、最終状態と書き出し内容で保存結果を確認する。
      await saveStatus.waitForDisplayed({ timeout: 5_000 });
    });
    await waitForText('[data-testid="editor-save-status"]', "保存済み", 15_000);

    const selectedRowSelector = `[role="row"][data-entry-id="${entryId}"]`;
    await browser.waitUntil(async () => (await $(selectedRowSelector).getAttribute("aria-selected")) === "true", {
      timeout: 5_000,
      timeoutMsg: "設定を開く前に翻訳行が選択されていません。",
    });
    const search = await $("#translation-search");
    await search.setValue(translationKey);
    await browser.waitUntil(async () => (await search.getValue()) === translationKey, { timeout: 5_000 });

    let settingsButton;
    for (const button of await $("nav.app-header__actions").$$("button")) {
      if (await button.getText() === "設定") {
        settingsButton = button;
        break;
      }
    }
    assert.ok(settingsButton, "Editorのヘッダーに設定がありません。");
    await settingsButton.click();
    await $(".settings-screen").waitForDisplayed({ timeout: 10_000 });
    await $(".settings-actions button").click();
    await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 10_000 });
    assert.equal(await search.getValue(), translationKey, "設定から戻ると検索語が消えています。");
    await browser.waitUntil(async () => (await $(selectedRowSelector).getAttribute("aria-selected")) === "true", {
      timeout: 5_000,
      timeoutMsg: "設定から戻ると選択行が失われています。",
    });

    await $('[data-testid="editor-export"]').click();
    const languageFileOption = await $('[for="export-option-languageFile"]');
    await languageFileOption.waitForClickable({ timeout: 30_000 });
    await languageFileOption.click();
    const namespaceSelect = await $('[role="combobox"][aria-label="名前空間"]');
    if (await namespaceSelect.isExisting()) {
      await namespaceSelect.click();
      await $('[role="option"][data-value="sample"]').click();
    }
    await $('[data-testid="export-start"]').click();
    await waitForText(".export-completion h1", "書き出しました", 30_000);
    let filenameRow;
    for (const row of await $$(".export-completion__details > div")) {
      if ((await row.$("dt").getText()) === "ファイル名") {
        filenameRow = row;
        break;
      }
    }
    assert.ok(filenameRow, "完了画面にファイル名が表示されるはずです。");
    assert.equal(await filenameRow.$("dd").getText(), "ja_jp.json");
    assert.equal(await $(".export-completion__instruction").getText(), "配置先: assets/sample/lang/");
    const completionCounts = await $(".export-completion__counts").getText();
    assert.match(completionCounts, /^\d+ 件 翻訳済み/);
    assert.match(completionCounts, /未翻訳 合計 \d+ 件/);

    const exported = JSON.parse(await readFile(exportPath, "utf8"));
    assert.deepEqual(exported, {
      "block.sample.machine": editedTranslation,
    });

    await browser.reloadSession();
    const recentProject = await $("button.recent-projects__open");
    await recentProject.waitForClickable({ timeout: 30_000 });
    await recentProject.click();
    await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 30_000 });
    const reopenedCell = await $(targetCellSelector);
    await reopenedCell.waitForDisplayed({ timeout: 30_000 });
    await browser.waitUntil(async () => (await readCellText(targetCellSelector)) === editedTranslation, {
      timeout: 10_000,
      interval: 100,
    });
    assert.equal(await readCellText(targetCellSelector), editedTranslation);
  });
});
