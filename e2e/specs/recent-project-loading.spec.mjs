import assert from "node:assert/strict";

describe("Recentからプロジェクトを開く", () => {
  it("読み込み画面を表示してからエディターへ進む", async () => {
    const recentProject = await $("button.recent-projects__open");
    await recentProject.waitForClickable({ timeout: 30_000 });

    // 小さなProjectではclickのWebDriver応答より先に読み込みが終わる。
    // 遷移前からDOMを観測し、製品側に待ち時間を足さず読み込み画面の表示順を確認する。
    await browser.execute(() => {
      const state = { loadingSeen: false, editorAfterLoading: false, startedAt: performance.now(), editorAt: null };
      const observer = new MutationObserver(() => {
        const loading = document.querySelector('[data-testid="project-loading"]');
        if (loading?.getClientRects().length) state.loadingSeen = true;
        const editor = document.querySelector('[aria-label="翻訳エディター"]');
        if (state.loadingSeen && editor?.getClientRects().length) {
          state.editorAfterLoading = true;
          state.editorAt ??= performance.now();
        }
      });
      observer.observe(document.body, { childList: true, subtree: true, attributes: true });
      window.__YAKUCRAFT_LOADING_OBSERVER__ = { state, observer };
    });
    try {
      await recentProject.click();
      await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 30_000 });
      const result = await browser.execute(() => window.__YAKUCRAFT_LOADING_OBSERVER__.state);
      assert.equal(result.loadingSeen, true, "読み込み画面が表示されませんでした。");
      assert.equal(result.editorAfterLoading, true, "読み込み画面の後にエディターが表示されませんでした。");
      console.log(`[performance] Recent click to editor displayed: ${Math.round(result.editorAt - result.startedAt)}ms`);
    } finally {
      await browser.execute(() => {
        window.__YAKUCRAFT_LOADING_OBSERVER__.observer.disconnect();
        delete window.__YAKUCRAFT_LOADING_OBSERVER__;
      });
    }
  });
});
