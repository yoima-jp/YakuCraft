import assert from "node:assert/strict";

describe("Linux Secret Service unavailable", () => {
  it("shows an actionable error and keeps the settings window open", async () => {
    let settingsButton;
    for (const button of await $("nav.app-header__actions").$$("button")) {
      if ((await button.getText()).trim() === "設定") settingsButton = button;
    }
    assert.ok(settingsButton, "ヘッダーに設定ボタンがありません。");

    await settingsButton.click();
    await $(".settings-screen").waitForDisplayed({ timeout: 15_000 });
    for (const button of await $('[aria-label="設定カテゴリ"]').$$("button")) {
      if ((await button.getText()).trim() === "AI翻訳") {
        await button.click();
        break;
      }
    }
    const error = await $("[role=alert]");
    await error.waitForDisplayed({ timeout: 10_000 });
    assert.match(await error.getText(), /GNOME Keyring/, "鍵管理サービスの起動を促すエラーが表示されていません。");
    assert.equal(await $(".settings-screen").isDisplayed(), true, "資格情報エラーでアプリが閉じています。");
  });
});
