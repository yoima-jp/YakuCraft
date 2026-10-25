import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import path from "node:path";

describe("Linux runtime", () => {
  it("切り替え可能な更新UI、日本語フォント、WebGLを確認する", async () => {
    const expectedAppImage = process.env.YAKUCRAFT_E2E_EXPECT_APPIMAGE === "1";
    let settingsButton;
    for (const button of await $("nav.app-header__actions").$$("button")) {
      if ((await button.getText()).trim() === "設定") settingsButton = button;
    }
    assert.ok(settingsButton, "ヘッダーに設定ボタンがありません。");
    await settingsButton.waitForClickable({ timeout: 15_000 });
    await settingsButton.click();
    await $(".settings-screen").waitForDisplayed({ timeout: 15_000 });
    const settingsText = await $(".settings-screen").getText();

    if (expectedAppImage) {
      assert.match(settingsText, /今すぐ確認/, "AppImageの更新確認UIが表示されていません。");
    } else {
      assert.match(settingsText, /パッケージを再インストールして更新してください/, "deb向けの更新案内が表示されていません。");
    }

    const runtime = await browser.execute(async () => {
      await document.fonts.ready;
      return {
        fontLoaded: [...document.fonts].some(
          (font) => font.family.includes("M PLUS Rounded 1c") && font.status === "loaded",
        ),
      };
    });
    assert.equal(runtime.fontLoaded, true, "日本語フォントがWebViewへ読み込まれていません。");

    const screenshotsDirectory = process.env.YAKUCRAFT_SCREENSHOTS_DIR;
    if (screenshotsDirectory) {
      await mkdir(screenshotsDirectory, { recursive: true });
      await browser.saveScreenshot(path.join(
        screenshotsDirectory,
        expectedAppImage ? "linux-settings-appimage.png" : "linux-settings-deb.png",
      ));
    }

    const fixturePath = process.env.YAKUCRAFT_E2E_FIXTURE;
    assert.ok(fixturePath, "fixture JAR のパスがありません。");
    await $("button=戻る").click();
    await $(".home-screen").waitForDisplayed({ timeout: 15_000 });
    await browser.execute((jarPath) => {
      window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, jarPath };
    }, fixturePath);
    await $('[data-testid="home-open-jar"]').click();
    await $(".review-screen h1").waitForDisplayed({ timeout: 60_000 });

    const reviewNext = $('[data-testid="review-next"]');
    let startedTranslation = false;
    for (let step = 0; step < 8; step += 1) {
      await reviewNext.waitForClickable({ timeout: 60_000 });
      const buttonText = (await reviewNext.getText()).trim();
      if (buttonText === "翻訳を始める") {
        await reviewNext.click();
        startedTranslation = true;
        break;
      }

      assert.equal(buttonText, "次へ", `Reviewの次の手順へ進めません: ${buttonText}`);
      const heading = await $(".review-screen h1").getText();
      await reviewNext.click();
      await browser.waitUntil(async () => {
        const currentHeading = await $(".review-screen h1");
        return !await currentHeading.isExisting() || (await currentHeading.getText()) !== heading;
      }, { timeout: 15_000, interval: 100, timeoutMsg: "Reviewの次の手順へ進みませんでした。" });
    }
    assert.equal(startedTranslation, true, "Reviewの全手順を終えられませんでした。");

    await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
    await $('[data-testid="translation-target-block.sample.machine"]').click();
    await $('[aria-label="アセットプレビュー"]').waitForDisplayed({ timeout: 30_000 });
    const preview = await browser.execute(() => {
      const section = document.querySelector('[aria-label="アセットプレビュー"]');
      const canvas = section?.querySelector("canvas.preview__canvas");
      const context = canvas?.getContext("webgl2");
      return {
        canvasExists: Boolean(canvas),
        width: canvas?.width ?? 0,
        height: canvas?.height ?? 0,
        webgl2Initialized: Boolean(context),
        renderError: section?.textContent?.includes("3Dプレビューを表示できません") ?? true,
      };
    });
    assert.equal(preview.canvasExists, true, "fixtureのプレビューcanvasがありません。");
    assert.ok(preview.width > 0 && preview.height > 0, "fixtureのプレビューcanvasが描画サイズになっていません。");
    assert.equal(preview.webgl2Initialized, true, "fixtureのプレビューでWebGL 2を初期化できません。");
    assert.equal(preview.renderError, false, "fixtureのプレビュー renderer またはモデル描画でエラーが発生しました。");
  });
});
