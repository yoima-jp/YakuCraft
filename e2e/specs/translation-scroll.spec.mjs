import assert from "node:assert/strict";

// 対象がない判断項目は省略されるため、ステップ数を決め打ちせず
// エディターが表示されるまで「次へ」を押し進める。
async function completeReview() {
  const editor = await $('[aria-label="翻訳エディター"]');
  for (let step = 0; step < 6; step += 1) {
    const heading = await $(".review-screen h1");
    await browser.waitUntil(async () => (await editor.isExisting()) || (await heading.isExisting()), { timeout: 60_000, interval: 100 });
    if (await editor.isExisting()) return;
    // 同じステップで二重に「次へ」を押さないよう、見出しが変わるまで待ってから次に進む。
    const previous = await heading.getText();
    await $('[data-testid="review-next"]').click();
    await browser.waitUntil(async () => (await editor.isExisting()) || (await $(".review-screen h1").getText().catch(() => "")) !== previous, { timeout: 30_000, interval: 100 });
  }
}

async function rectOf(element) {
  return browser.execute((node) => {
    const rect = node.getBoundingClientRect();
    return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
  }, element);
}

describe("翻訳表の大量行スクロール", () => {
  it("500キーの末尾までスクロールでき、各行のサムネイルとInspectorを保つ", async () => {
    const fixturePath = process.env.YAKUCRAFT_E2E_LARGE_FIXTURE;
    assert.ok(fixturePath, "500キーfixtureのパスがありません。");

    await browser.execute((jarPath) => {
      window.__YAKUCRAFT_E2E__ = { jarPath };
    }, fixturePath);
    await $('[data-testid="home-open-jar"]').waitForClickable({ timeout: 30_000 });
    await $('[data-testid="home-open-jar"]').click();

    await completeReview();

    await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
    const grid = await $('[role="grid"][aria-label="翻訳一覧"]');
    await browser.waitUntil(async () => Number(await grid.getAttribute("aria-rowcount")) >= 501, {
      timeout: 30_000,
      interval: 100,
      timeoutMsg: "500件の翻訳行が読み込まれませんでした。",
    });
    assert.equal(Number(await grid.getAttribute("aria-rowcount")), 501, "ヘッダーを含む500キーが表示されている必要があります。");

    const firstPreviewCell = await $('[data-testid="translation-target-block.sample.machine"]');
    await firstPreviewCell.waitForDisplayed({ timeout: 30_000 });
    const firstThumbnail = await $('[data-testid="translation-thumbnail-block.sample.machine"]');
    await firstThumbnail.waitForDisplayed({ timeout: 5_000 });
    const firstThumbnailFrame = await $('[data-testid="translation-thumbnail-block.sample.machine"] .editor-table__thumbnail-frame');
    await firstPreviewCell.click();
    await $(".editor-inspector__preview-area").waitForDisplayed({ timeout: 5_000 });

    const thumbnailBefore = await rectOf(firstThumbnailFrame);
    const inspectorPreview = await $(".editor-inspector__preview-area");
    const previewTop = (await rectOf(inspectorPreview)).y;
    const scrollRect = await rectOf(await $(".editor-table__scroll"));
    for (let attempt = 0; attempt < 30; attempt += 1) {
      const reachedBottom = await browser.execute(() => {
        const scroller = document.querySelector(".editor-table__scroll");
        return Boolean(scroller && scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2);
      });
      if (reachedBottom) break;
      await browser.performActions([{
        type: "wheel",
        id: "table-scroll",
        actions: [{
          type: "scroll",
          x: Math.round(scrollRect.x + scrollRect.width / 2),
          y: Math.round(scrollRect.y + scrollRect.height / 2),
          deltaX: 0,
          deltaY: 1_200,
          duration: 100,
        }],
      }]);
      await browser.releaseActions();
    }

    const rowCount = Number(await grid.getAttribute("aria-rowcount"));
    const lastRow = await $(`[role="row"][aria-rowindex="${rowCount}"]`);
    await lastRow.waitForDisplayed({ timeout: 10_000 });
    await browser.waitUntil(async () => await browser.execute(() => {
      const scroller = document.querySelector(".editor-table__scroll");
      return Boolean(scroller && scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2);
    }), { timeout: 5_000, interval: 100, timeoutMsg: "翻訳表が末尾までスクロールしませんでした。" });

    const lastThumbnail = await $(`[role="row"][aria-rowindex="${rowCount}"] .editor-table__thumbnail-frame`);
    assert.equal(await lastThumbnail.isDisplayed(), true, "表示中の行にサムネイル枠がありません。");
    assert.equal((await rectOf(lastThumbnail)).x, thumbnailBefore.x, "表の縦スクロールでサムネイル列が横に動いています。");
    assert.equal((await rectOf(inspectorPreview)).y, previewTop, "表のスクロールでInspectorのPreviewが動いています。");
    assert.equal(await browser.execute(() => document.scrollingElement?.scrollTop ?? -1), 0, "bodyがスクロールしています。");

    await browser.execute(() => {
      const scroller = document.querySelector(".editor-table__scroll");
      if (scroller) scroller.scrollTop = 0;
    });
    await firstPreviewCell.waitForDisplayed({ timeout: 5_000 });
    await firstPreviewCell.click();
    await browser.keys("End");
    const lastIndex = rowCount;
    await browser.waitUntil(async () => {
      const activeIndex = await browser.execute(() => {
        const activeCell = document.activeElement?.closest("[role=gridcell]");
        return activeCell?.closest("[role=row]")?.getAttribute("aria-rowindex") ?? null;
      });
      return activeIndex === String(lastIndex);
    }, { timeout: 5_000, interval: 100, timeoutMsg: "Endキーで末尾の行にフォーカスできませんでした。" });
    const focusedLastCell = await $(`[role="row"][aria-rowindex="${lastIndex}"] [role="gridcell"][tabindex="0"]`);
    await focusedLastCell.waitForDisplayed({ timeout: 5_000 });
    await browser.keys("Home");
    await browser.waitUntil(async () => await browser.execute(() => {
      const activeCell = document.activeElement?.closest("[role=gridcell]");
      return activeCell?.closest("[role=row]")?.getAttribute("aria-rowindex") === "2";
    }), { timeout: 5_000, interval: 100, timeoutMsg: "Homeキーで先頭の行にフォーカスできませんでした。" });
  });
});
