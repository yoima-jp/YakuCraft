import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { generateFeatureFixtures } from "../scripts/generate-feature-fixtures.mjs";

const screenshotsRoot = process.env.YAKUCRAFT_SCREENSHOTS_DIR
  ? path.resolve(process.env.YAKUCRAFT_SCREENSHOTS_DIR)
  : fileURLToPath(new URL("../screenshots/", import.meta.url));
const settingsCategories = [
  ["一般", "settings-general"],
  ["AI翻訳", "settings-ai"],
  ["エディター", "settings-editor"],
  ["表示", "settings-appearance"],
  ["詳細", "settings-advanced"],
];
const crcTable = Uint32Array.from({ length: 256 }, (_, value) => {
  let crc = value;
  for (let bit = 0; bit < 8; bit += 1) {
    crc = (crc & 1) ? (0xedb88320 ^ (crc >>> 1)) : (crc >>> 1);
  }
  return crc >>> 0;
});

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function createStoredZip(entries) {
  const localRecords = [];
  const centralRecords = [];
  let offset = 0;

  for (const [filePath, data] of entries) {
    const name = Buffer.from(filePath, "utf8");
    const checksum = crc32(data);
    const local = Buffer.alloc(30 + name.length);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6);
    local.writeUInt16LE(33, 12);
    local.writeUInt32LE(checksum, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    name.copy(local, 30);
    localRecords.push(local, data);

    const central = Buffer.alloc(46 + name.length);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0x0800, 8);
    central.writeUInt16LE(33, 14);
    central.writeUInt32LE(checksum, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt32LE(offset, 42);
    name.copy(central, 46);
    centralRecords.push(central);
    offset += local.length + data.length;
  }

  const centralDirectory = Buffer.concat(centralRecords);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralDirectory.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...localRecords, centralDirectory, end]);
}

function createScreenshotModJar({ id, name, namespace, source, japanese }) {
  const json = (value) => Buffer.from(JSON.stringify(value), "utf8");
  const entries = [
    ["fabric.mod.json", json({
      schemaVersion: 1,
      id,
      name,
      version: "1.0.0",
      depends: { minecraft: "1.21.1" },
    })],
    [`assets/${namespace}/lang/en_us.json`, json(source)],
  ];
  if (japanese) entries.push([`assets/${namespace}/lang/ja_jp.json`, json(japanese)]);
  return createStoredZip(entries);
}

async function createMultiModFixture(instancePath) {
  const modsPath = path.join(instancePath, "mods");
  await mkdir(modsPath, { recursive: true });
  await writeFile(path.join(instancePath, "options.txt"), "version:1\n", "utf8");

  const mods = [
    {
      id: "gearworks",
      name: "Gearworks",
      namespace: "gearworks",
      source: {
        "block.gearworks.machine": "Machine",
        "item.gearworks.gear": "Gear",
        "tooltip.gearworks.progress": "Progress: %s",
        "item.gearworks.pressure_plate": "Pressure Plate",
      },
      japanese: {
        "block.gearworks.machine": "機械",
        "item.gearworks.gear": "歯車",
        "tooltip.gearworks.progress": "進捗: %d",
      },
    },
    {
      id: "wildlands",
      name: "Wildlands",
      namespace: "wildlands",
      source: { "item.wildlands.berry": "Berry", "entity.wildlands.fox": "Fox" },
      japanese: { "item.wildlands.berry": "ベリー", "entity.wildlands.fox": "キツネ" },
    },
    {
      id: "stargazer",
      name: "Stargazer",
      namespace: "stargazer",
      source: { "item.stargazer.telescope": "Telescope", "block.stargazer.observatory": "Observatory" },
      japanese: null,
    },
    {
      id: "beaconry",
      name: "Beaconry",
      namespace: "beaconry",
      source: { "block.beaconry.lens": "Lens", "item.beaconry.prism": "Prism" },
      japanese: { "block.beaconry.lens": "レンズ" },
    },
  ];

  await Promise.all(mods.map(async (mod) => {
    await writeFile(path.join(modsPath, `${mod.id}.jar`), createScreenshotModJar(mod));
  }));
}

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

async function waitForTheme(theme) {
  await browser.waitUntil(
    async () => await browser.execute(() => document.documentElement.dataset.theme) === theme,
    { timeout: 10_000, interval: 100, timeoutMsg: `${theme}テーマに切り替わりませんでした。` },
  );
}

function startScreenshotModelServer() {
  const requests = [];
  const server = createServer((request, response) => {
    requests.push(`${request.method} ${request.url}`);
    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ data: [{ id: "screenshot-model" }] }));
      return;
    }
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not found" }));
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("モデル一覧用モックサーバーのポートを取得できませんでした。"));
        return;
      }
      resolve({ server, requests, url: `http://127.0.0.1:${address.port}` });
    });
  });
}

async function capture(theme, name, settleDelay = 200) {
  if (settleDelay > 0) await browser.pause(settleDelay);
  const directory = path.join(screenshotsRoot, theme);
  await mkdir(directory, { recursive: true });
  await browser.saveScreenshot(path.join(directory, `${name}.png`));
}

async function captureResponsiveEditorLayouts(theme) {
  const originalSize = await browser.getWindowSize();
  const toggle = await $('[data-testid="editor-toggle-inspector"]');

  async function setInspectorOpen(open) {
    const expanded = await toggle.getAttribute("aria-expanded") === "true";
    if (expanded !== open) await toggle.click();

    await browser.waitUntil(async () => {
      const layout = await browser.execute(() => {
        const inspector = document.querySelector(".editor-inspector");
        const scroll = document.querySelector(".editor-table__scroll");
        const content = document.querySelector(".editor-table__content");
        if (!inspector || !scroll || !content) return null;
        return {
          inspectorOpen: !inspector.hasAttribute("hidden"),
          viewportWidth: scroll.clientWidth,
          tableWidth: content.getBoundingClientRect().width,
          scrollWidth: scroll.scrollWidth,
        };
      });
      return layout?.inspectorOpen === open
        && layout.viewportWidth > 0
        && Math.abs(layout.tableWidth - layout.viewportWidth) < 2;
    }, {
      timeout: 5_000,
      interval: 100,
      timeoutMsg: `Inspector ${open ? "open" : "closed"} の表幅が追従しませんでした。`,
    });

    const layout = await browser.execute(() => {
      const scroll = document.querySelector(".editor-table__scroll");
      const content = document.querySelector(".editor-table__content");
      return scroll && content
        ? { viewportWidth: scroll.clientWidth, scrollWidth: scroll.scrollWidth, tableWidth: content.getBoundingClientRect().width }
        : null;
    });
    assert.ok(layout, "翻訳表の幅を取得できませんでした。");
    assert.ok(Math.abs(layout.tableWidth - layout.viewportWidth) < 2, "表の右端に不要な空白があります。");
    assert.ok(layout.scrollWidth <= layout.viewportWidth + 2, "既定列幅で不要な横スクロールが発生しました。");
    return layout;
  }

  try {
    for (const width of [1100, 1280, 1920]) {
      await browser.setWindowSize(width, 700);
      const openLayout = await setInspectorOpen(true);
      await capture(theme, `editor-responsive-${width}-inspector-open`);
      const closedLayout = await setInspectorOpen(false);
      await capture(theme, `editor-responsive-${width}-inspector-closed`);
      // 狭い幅ではInspectorを閉じて表を広げ、広い幅ではInspectorを開いたまま使える。
      if (width > 1100) {
        assert.ok(closedLayout.viewportWidth > openLayout.viewportWidth, `${width}pxでInspectorを閉じても表が広がりませんでした。`);
      }
    }
  } finally {
    await browser.setWindowSize(originalSize.width, originalSize.height);
    await setInspectorOpen(true);
  }
}

async function tabUntil(selector) {
  for (let step = 0; step < 32; step += 1) {
    const focused = await browser.execute((target) => (
      document.activeElement instanceof Element && document.activeElement.matches(target)
    ), selector);
    if (focused) return;
    await browser.keys("Tab");
  }
  assert.fail(`Tab移動で「${selector}」にフォーカスできませんでした。`);
}

async function selectSettingsCategory(label) {
  const buttons = await $$(".settings-nav__item");
  for (const button of buttons) {
    if (await button.getText() === label) {
      await button.click();
      await waitForText(".settings-content > h2", label);
      return;
    }
  }
  assert.fail(`設定カテゴリ「${label}」がありません。`);
}

async function selectAiProvider(value, label) {
  await $('[role="combobox"][aria-label="プロバイダー"]').click();
  await $(`[role="option"][data-value="${value}"]`).click();
  await browser.waitUntil(async () => (
    await $('[role="combobox"][aria-label="プロバイダー"]').getText()
  ).includes(label), {
    timeout: 15_000,
    interval: 100,
    timeoutMsg: `プロバイダー「${label}」を選べませんでした。`,
  });
}

async function captureSettings(theme, modelServerUrl) {
  await $("nav.app-header__actions").$("button=設定").click();
  await $(".settings-screen").waitForDisplayed({ timeout: 10_000 });

  // 設定画面からテーマを切り替え、他のカテゴリも同じ配色で撮影する。
  await selectSettingsCategory("表示");
  if (theme === "light") {
    await $('[for="theme-light"]').click();
  } else {
    await $('[for="theme-dark"]').click();
    await browser.waitUntil(
      async () => await browser.execute(() => document.querySelector("#theme-dark")?.checked === true),
      { timeout: 5_000, timeoutMsg: "ダークテーマが選択されませんでした。" },
    );
  }
  await waitForTheme(theme);
  await capture(theme, "settings-appearance");

  for (const [label, name] of settingsCategories) {
    if (label === "表示") continue;
    await selectSettingsCategory(label);
    await capture(theme, name);
  }

  await selectSettingsCategory("AI翻訳");
  const providerSelect = await $('[role="combobox"][aria-label="プロバイダー"]');
  await providerSelect.click();
  await $('[role="listbox"][aria-label="プロバイダー"]').waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "settings-ai-select-open");
  await browser.keys("Escape");

  await selectAiProvider("anthropic", "Claude（Anthropic）");
  await capture(theme, "settings-ai-claude");
  await selectAiProvider("ollama", "Ollama");
  assert.equal(await $('[type="password"]').isExisting(), false, "Ollama にAPIキー欄が表示されています。");
  await capture(theme, "settings-ai-ollama");

  await selectAiProvider("openai-compatible", "OpenAI 互換（カスタム）");
  await $('[type="url"]').setValue(`${modelServerUrl}/v1`);
  const modelInput = await $("#ai-model");
  await modelInput.setValue("screenshot-model");
  await $("button=保存").click();
  await modelInput.setValue("screenshot-model");
  await modelInput.click();
  await browser.keys("ArrowDown");
  await capture(theme, "settings-ai-models");
  await modelInput.setValue("manually-entered-model");
  await capture(theme, "settings-ai-model-manual");
}

// 標準fixtureにはブロック状態がないため、Variant Selectorの撮影用JARを一時生成する。
async function createVariantFixture(outputPath) {
  const json = (value) => Buffer.from(JSON.stringify(value), "utf8");
  await writeFile(outputPath, createStoredZip([
    ["fabric.mod.json", json({ schemaVersion: 1, id: "sample", name: "Variant Fixture", version: "1.0" })],
    ["assets/sample/lang/en_us.json", json({ "block.sample.machine": "Machine" })],
    ["assets/sample/lang/ja_jp.json", json({ "block.sample.machine": "機械" })],
    ["assets/sample/blockstates/machine.json", json({
      variants: Object.fromEntries(["north", "east", "south", "west"].map((facing) => [
        `facing=${facing}`,
        { model: "sample:block/machine" },
      ])),
    })],
    ["assets/sample/models/block/machine.json", json({
      parent: "minecraft:block/cube_all",
      textures: { all: "sample:block/machine" },
    })],
    ["assets/sample/textures/block/machine.png", Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADUlEQVR4nGNgYGAAAAAEAAEnNCcKAAAAAElFTkSuQmCC",
      "base64",
    )],
  ]));
}

async function captureProjectFlow(theme, fixturePath) {
  await browser.execute((jarPath) => {
    window.__YAKUCRAFT_E2E__.jarPath = jarPath;
  }, fixturePath);
  await capture(theme, "home");
  await capture(theme, "home-shaderpack");
  await $('[data-testid="home-open-jar"]').click();
  // 解析中の進捗と中止操作も撮り、画面の見た目を確認できるようにする。
  await $(".analyze-screen").waitForDisplayed({ timeout: 10_000 });
  await capture(theme, "analyze", 0);

  // 対象がない判断項目は省略されるため、表示されたReview画面を順に撮る。
  const editor = await $('[aria-label="翻訳エディター"]');
  for (let step = 1; step <= 6; step += 1) {
    const heading = await $(".review-screen h1");
    await browser.waitUntil(async () => (await editor.isExisting()) || (await heading.isExisting()), { timeout: 60_000, interval: 100 });
    if (await editor.isExisting()) break;
    const previous = await heading.getText();
    await capture(theme, `review-${step}`);
    await $('[data-testid="review-next"]').click();
    await browser.waitUntil(async () => (await editor.isExisting()) || (await $(".review-screen h1").getText().catch(() => "")) !== previous, { timeout: 30_000, interval: 100 });
  }

  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
  const targetCell = await $('[data-testid="translation-target-block.sample.machine"]');
  await targetCell.waitForDisplayed({ timeout: 30_000 });
  await targetCell.click();
  const entryId = await targetCell.getAttribute("data-entry-id");
  const selectedRow = await $(`[role="row"][data-entry-id="${entryId}"]`);
  await browser.waitUntil(async () => (await selectedRow.getAttribute("aria-selected")) === "true", {
    timeout: 5_000,
    timeoutMsg: "Editorの翻訳行が選択されませんでした。",
  });
  await capture(theme, "editor-selected");
  await capture(theme, "editor-add-units-single");
  await captureResponsiveEditorLayouts(theme);

  await $(".editor-inspector__scroll").waitForDisplayed({ timeout: 5_000 });
  await browser.execute(() => {
    const scroller = document.querySelector(".editor-inspector__scroll");
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  });
  await browser.waitUntil(async () => await browser.execute(() => {
    const scroller = document.querySelector(".editor-inspector__scroll");
    return Boolean(scroller && scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2);
  }), { timeout: 5_000, interval: 100, timeoutMsg: "Inspectorが末尾までスクロールしませんでした。" });
  await capture(theme, "editor-inspector-scrolled");
  await browser.execute(() => {
    const scroller = document.querySelector(".editor-inspector__scroll");
    if (scroller) scroller.scrollTop = 0;
  });
  await browser.waitUntil(async () => await browser.execute(() => {
    const scroller = document.querySelector(".editor-inspector__scroll");
    return scroller?.scrollTop === 0;
  }), {
    timeout: 5_000,
    interval: 100,
  });

  await $(".editor-inspector__preview-area").waitForDisplayed({ timeout: 10_000 });
  await $('[aria-label="プレビューを非表示"]').click();
  const showPreview = await $('[aria-label="プレビューを表示"]');
  await showPreview.waitForDisplayed({ timeout: 5_000 });
  await browser.waitUntil(async () => await browser.execute(() => !document.querySelector(".editor-inspector__preview-area")), {
    timeout: 5_000,
    interval: 100,
    timeoutMsg: "プレビューを非表示にした後もプレビュー領域が残っています。",
  });
  await capture(theme, "editor-preview-hidden");
  await showPreview.click();
  await $('[aria-label="プレビューを非表示"]').waitForDisplayed({ timeout: 5_000 });

  await browser.execute(() => {
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  });
  await tabUntil('[data-testid="editor-export"]');
  await capture(theme, "editor-focus-button");

  const filterMenu = await $(".editor-toolbar__filters > .menu__trigger");
  await filterMenu.click();
  await browser.waitUntil(async () => (await filterMenu.getAttribute("aria-expanded")) === "true", {
    timeout: 5_000,
    timeoutMsg: "翻訳フィルターが開きませんでした。",
  });
  await capture(theme, "editor-filter-menu-open");
  const statusSelect = await $('[role="combobox"][aria-label="状態"]');
  await statusSelect.waitForDisplayed({ timeout: 5_000 });
  await browser.keys("Tab");
  await browser.waitUntil(async () => await browser.execute(() => document.activeElement?.getAttribute("aria-label")) === "状態", {
    timeout: 5_000,
    timeoutMsg: "キーボードで状態セレクトに移動しませんでした。",
  });
  await capture(theme, "editor-filter-select-focused");
  await browser.keys("Enter");
  await $('[role="listbox"][aria-label="状態"]').waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-filter-select-open");
  await browser.keys("Escape");

  const aiMenu = await $(".editor-toolbar__ai-menu > .menu__trigger");
  await aiMenu.click();
  await browser.waitUntil(async () => (await aiMenu.getAttribute("aria-expanded")) === "true", {
    timeout: 5_000,
    timeoutMsg: "AI翻訳メニューが開きませんでした。",
  });
  await $(".editor-toolbar__ai-menu-panel").$("button=未翻訳を翻訳").moveTo();
  await capture(theme, "editor-ai-menu-open");
  await browser.keys("Escape");

  const homeButton = await $('nav.app-header__actions [aria-label="ホームへ戻る"]');
  await homeButton.moveTo();
  await browser.waitUntil(async () => await browser.execute(() => {
    const tooltip = [...document.querySelectorAll(".tooltip__content")]
      .find((element) => element.dataset.visible === "true");
    return tooltip?.textContent?.includes("ホームへ戻る") ?? false;
  }), {
    timeout: 5_000,
    timeoutMsg: "ヘッダーの戻るボタンにツールチップが表示されませんでした。",
  });
  await capture(theme, "editor-header-tooltip");

  const variantSelect = await $(".preview__variant-list [role=combobox]");
  if (await variantSelect.isExisting()) {
    await variantSelect.click();
    await $(".select__menu[role=listbox]").waitForDisplayed({ timeout: 5_000 });
    await capture(theme, "editor-variant-select-open");
    await browser.keys("Escape");
  }

  await targetCell.click();
  await browser.keys("Enter");
  const longEditorSelector = `textarea[aria-label="block.sample.machine の日本語訳"]`;
  const longEditor = await $(longEditorSelector);
  await longEditor.waitForDisplayed({ timeout: 5_000 });
  await browser.execute((text) => {
    const textarea = document.activeElement;
    if (!(textarea instanceof HTMLTextAreaElement)) throw new Error("翻訳欄にフォーカスがありません。");
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set?.call(textarea, text);
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
  }, "機械を操作すると資源を加工できます。長い説明文を入力する間は、セルの位置から編集欄が広がり、原文と見比べながら文章を整えられます。");
  await $(".editor-table__editor-overlay").waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-long-edit");
  await browser.keys("Escape");
  await browser.waitUntil(async () => !(await $(longEditorSelector).isExisting()), {
    timeout: 5_000,
    interval: 100,
    timeoutMsg: "Escape後に編集欄が閉じませんでした。",
  });

  await browser.execute(() => {
    const scroller = document.querySelector(".editor-table__scroll");
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  });
  await browser.waitUntil(async () => await browser.execute(() => {
    const scroller = document.querySelector(".editor-table__scroll");
    return Boolean(scroller && scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2);
  }), { timeout: 5_000, interval: 100, timeoutMsg: "スクリーンショット用の表が末尾までスクロールしませんでした。" });
  const rowCount = await $('[role="grid"]').getAttribute("aria-rowcount");
  await $(`[role="row"][aria-rowindex="${rowCount}"] .editor-table__thumbnail-frame`).waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-scrolled-bottom");

  await $('[data-testid="editor-export"]').click();
  await $(".export-screen").waitForDisplayed({ timeout: 10_000 });
  await $(".export-validation").waitForDisplayed({ timeout: 10_000 });
  await capture(theme, "export");

  await $('[for="export-option-resourcePack"]').click();
  const minecraftVersionSelect = await $('[role="combobox"][aria-label="Minecraft バージョン"]');
  await minecraftVersionSelect.waitForDisplayed({ timeout: 10_000 });
  await minecraftVersionSelect.click();
  await $('[role="listbox"][aria-label="Minecraft バージョン"]').waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "export-minecraft-version-select-open");
  await browser.keys("Escape");
  await $('[data-testid="export-start"]').click();
  await $(".export-completion").waitForDisplayed({ timeout: 60_000 });
  await capture(theme, "export-complete");
}

async function openSettingsFromHeader() {
  for (const button of await $("nav.app-header__actions").$$("button")) {
    if ((await button.getText()).trim() === "設定") {
      await button.click();
      await $(".settings-screen").waitForDisplayed({ timeout: 10_000 });
      return;
    }
  }
  assert.fail("ヘッダーに設定ボタンがありません。");
}

async function setModTabPosition(position) {
  await openSettingsFromHeader();
  await selectSettingsCategory("エディター");
  const select = await $('[aria-label="modタブの位置"]');
  await select.click();
  await $(`[role="option"][data-value="${position}"]`).click();
  await waitForText('[aria-label="modタブの位置"]', position === "left" ? "左" : "上");
  await $("footer.settings-actions button").click();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 10_000 });
}

async function advanceReviewToEditor() {
  const editor = await $('[aria-label="翻訳エディター"]');
  const next = await $('[data-testid="review-next"]');
  for (let step = 0; step < 12; step += 1) {
    if (await editor.isExisting()) return;
    const previousHeading = (await $(".review-screen h1").getText()).trim();
    await next.waitForClickable({ timeout: 30_000 });
    await next.click();
    await browser.waitUntil(async () => (
      await editor.isExisting()
      || (await $(".review-screen h1").getText().catch(() => "")).trim() !== previousHeading
    ), { timeout: 30_000, interval: 100, timeoutMsg: "Reviewの次の画面へ進みませんでした。" });
  }
  assert.fail("Reviewの手順を終えられませんでした。");
}

async function selectScreenshotModTab(name) {
  const tabSelector = '[role="tablist"][aria-label="開いているmod"] [role="tab"]';
  let selector = tabSelector;
  const hasMatchingTab = await browser.execute((targetSelector, targetName) => (
    [...document.querySelectorAll(targetSelector)]
      .some((element) => element.textContent.includes(targetName))
  ), tabSelector, name);
  if (!hasMatchingTab) {
    await $(".mod-tabs__all-button").click();
    await $(".mod-tabs__panel").waitForDisplayed({ timeout: 5_000 });
    selector = ".mod-tabs__open-unit";
  }
  await browser.execute((targetSelector, targetName) => {
    const target = [...document.querySelectorAll(targetSelector)]
      .find((element) => element.textContent.includes(targetName));
    if (!target) throw new Error(`mod tab not found: ${targetName}`);
    target.click();
  }, selector, name);
  await browser.waitUntil(async () => browser.execute((targetSelector, targetName) => (
    document.querySelector(targetSelector)?.textContent.includes(targetName) ?? false
  ), tabSelector + '[aria-selected="true"]', name), { timeout: 10_000, interval: 100 });
}

async function captureMultiModFlow(theme, instancePath) {
  await setModTabPosition("top");
  await $('.app-brand[aria-label="ホームへ戻る"]').click();
  await $('[data-testid="home-open-mods-folder"]').waitForDisplayed({ timeout: 10_000 });
  await browser.execute((directoryPath) => {
    window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, directoryPath };
  }, instancePath);
  await $('[data-testid="home-open-mods-folder"]').click();
  await browser.waitUntil(async () => (await $$(".scan-screen__row")).length === 4, {
    timeout: 60_000,
    interval: 100,
    timeoutMsg: "4個のfixture modがスキャン結果に表示されませんでした。",
  });
  await capture(theme, "scan-results");

  const createProject = await $(".scan-screen__actions button");
  await createProject.waitForClickable({ timeout: 10_000 });
  await createProject.click();
  await $(".review-screen h1").waitForDisplayed({ timeout: 60_000 });
  await waitForText(".review-screen h1", "Mod情報を確認");
  await capture(theme, "review-mod-information");
  await advanceReviewToEditor();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });

  await selectScreenshotModTab("Gearworks");
  const targetCell = await $('[data-testid="translation-target-block.gearworks.machine"]');
  await targetCell.waitForDisplayed({ timeout: 30_000 });
  await targetCell.click();
  const entryId = await targetCell.getAttribute("data-entry-id");
  const selectedRow = await $(`[role="row"][data-entry-id="${entryId}"]`);
  await browser.waitUntil(async () => (await selectedRow.getAttribute("aria-selected")) === "true", {
    timeout: 5_000,
    timeoutMsg: "翻訳行が選択されませんでした。",
  });
  await $(".editor-selection-toolbar--visible").waitForDisplayed({ timeout: 5_000 });
  await browser.execute(() => window.scrollTo(0, 0));
  await capture(theme, "editor-multiple-top-selection");

  let glossaryButton;
  for (const button of await $$("button")) {
    if ((await button.getText()).trim() === "用語集") {
      glossaryButton = button;
      break;
    }
  }
  assert.ok(glossaryButton, "用語集ボタンがありません。");
  await glossaryButton.click();
  const glossary = await $('[role="dialog"] .glossary-panel');
  await glossary.waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-glossary-panel");
  await glossary.$('[aria-label="閉じる"]').click();

  await selectedRow.click({ button: "right" });
  await $(".context-menu").waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-multiple-top-context-menu");
  await browser.keys("Escape");
  await $(".mod-tabs__all-button").click();
  await $("dialog.mod-tabs__panel").waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-all-mods-panel");
  await browser.keys("Escape");

  await setModTabPosition("left");
  await $(".mod-tabs--vertical").waitForDisplayed({ timeout: 10_000 });
  await browser.execute(() => window.scrollTo(0, 0));
  await capture(theme, "editor-vertical-tabs");

  await $('[data-testid="editor-export"]').click();
  await $(".export-screen").waitForDisplayed({ timeout: 10_000 });
  await $(".export-validation").waitForDisplayed({ timeout: 10_000 });
  await browser.waitUntil(async () => await browser.execute(() => (
    document.querySelectorAll(".export-validation__details").length
  )) === 2, {
    timeout: 15_000,
    interval: 100,
    timeoutMsg: "複数modの書き出し検証が表示されませんでした。",
  });
  const validationDetailCount = await browser.execute(() => (
    document.querySelectorAll(".export-validation__details").length
  ));
  assert.equal(validationDetailCount, 2, "未翻訳とplaceholderの確認項目が表示されませんでした。");
  for (let index = 1; index <= validationDetailCount; index += 1) {
    const validationDetails = await $(`.export-validation__details:nth-of-type(${index})`);
    await validationDetails.waitForDisplayed({ timeout: 15_000 });
    await validationDetails.$("summary").click();
    await browser.waitUntil(async () => await validationDetails.getAttribute("open") !== null, {
      timeout: 5_000,
      interval: 100,
      timeoutMsg: "書き出し検証の詳細が開きませんでした。",
    });
  }
  await browser.execute(() => {
    document.querySelector(".export-validation")
      ?.scrollIntoView({ block: "center", behavior: "instant" });
  });
  await browser.waitUntil(async () => await browser.execute(() => {
    const details = [...document.querySelectorAll(".export-validation__details")];
    return details.length === 2 && details.every((element) => {
      const bounds = element.getBoundingClientRect();
      return bounds.top >= 0 && bounds.bottom <= window.innerHeight;
    });
  }), { timeout: 5_000, interval: 100, timeoutMsg: "未翻訳とPlaceholderの検証欄が画面内に収まりませんでした。" });
  await capture(theme, "export-multiple-mods-validation");
  await $("footer.export-screen__actions").$("button=戻る").click();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 10_000 });

  await $('.app-brand[aria-label="ホームへ戻る"]').click();
  await $("li.recent-projects__item:first-child").waitForDisplayed({ timeout: 10_000 });
  await capture(theme, "home-recent-projects");
  const recentProject = await $("li.recent-projects__item:first-child");
  await browser.execute((element) => element.focus(), recentProject);
  assert.equal(await recentProject.isFocused(), true, "Recent項目にフォーカスできませんでした。");
  await browser.keys(["Shift", "F10"]);
  await $(".context-menu").waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "home-recent-project-context-menu");
  await browser.keys("Escape");
  await $("li.recent-projects__item:first-child > button.recent-projects__open").click();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
}

async function captureVariantSelector(theme, fixturePath) {
  await browser.execute((jarPath) => {
    window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, jarPath };
  }, fixturePath);
  await $('.app-brand[aria-label="ホームへ戻る"]').click();
  await $('[data-testid="home-open-jar"]').waitForDisplayed({ timeout: 10_000 });
  await $('[data-testid="home-open-jar"]').click();
  await $(".analyze-screen").waitForDisplayed({ timeout: 10_000 });

  const editor = await $('[aria-label="翻訳エディター"]');
  for (let step = 0; step < 6; step += 1) {
    const heading = await $(".review-screen h1");
    await browser.waitUntil(async () => (await editor.isExisting()) || (await heading.isExisting()), {
      timeout: 60_000,
      interval: 100,
    });
    if (await editor.isExisting()) break;
    const previous = await heading.getText();
    await $('[data-testid="review-next"]').click();
    await browser.waitUntil(async () => (await editor.isExisting()) || (await $(".review-screen h1").getText().catch(() => "")) !== previous, {
      timeout: 30_000,
      interval: 100,
    });
  }

  await editor.waitForDisplayed({ timeout: 60_000 });
  const targetCell = await $('[data-testid="translation-target-block.sample.machine"]');
  await targetCell.waitForDisplayed({ timeout: 30_000 });
  await targetCell.click();
  const facing = await $('[role="combobox"][aria-label="向き"]');
  await facing.waitForDisplayed({ timeout: 10_000 });
  await facing.click();
  await $('[role="listbox"][aria-label="向き"]').waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-variant-select-open");
  await browser.keys("Escape");
}

async function scanGeneratedInstance(instancePath) {
  await browser.execute((directoryPath) => {
    window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, directoryPath };
  }, instancePath);
  await $('[data-testid="home-open-mods-folder"]').click();
  await $(".scan-screen").waitForDisplayed({ timeout: 60_000 });
  await browser.waitUntil(async () => (await $$(".scan-screen__row")).length > 0, {
    timeout: 60_000,
    interval: 100,
    timeoutMsg: "生成したfixtureのスキャン結果が表示されませんでした。",
  });
}

async function findScanRow(name) {
  let matchingRow;
  await browser.waitUntil(async () => {
    for (const row of await $$(".scan-screen__row")) {
      if ((await row.getText()).includes(name)) {
        matchingRow = row;
        return true;
      }
    }
    return false;
  }, {
    timeout: 60_000,
    interval: 100,
    timeoutMsg: `スキャン結果に「${name}」がありません。`,
  });
  return matchingRow;
}

async function selectScanRow(name, selected) {
  const row = await findScanRow(name);
  const checkbox = await row.$('input[type="checkbox"]');
  assert.equal(await checkbox.isEnabled(), true, `${name} は選択できるはずです。`);
  if (await checkbox.isSelected() !== selected) await row.$("label").click();
  assert.equal(await checkbox.isSelected(), selected);
}

async function returnHome() {
  await $('.app-brand[aria-label="ホームへ戻る"]').click();
  await $(".home-screen").waitForDisplayed({ timeout: 15_000 });
}

async function editScreenshotTranslation(key, value) {
  const target = await $(`[data-testid="translation-target-${key}"]`);
  await target.waitForDisplayed({ timeout: 30_000 });
  await target.click();
  await browser.keys("Enter");
  const editor = await $(`textarea[aria-label="${key} の日本語訳"]`);
  await editor.waitForDisplayed({ timeout: 5_000 });
  await browser.keys(["Control", "A"]);
  await browser.keys(value);
  await browser.keys("Enter");
  await browser.waitUntil(async () => (await target.getText()) === value, {
    timeout: 10_000,
    interval: 100,
    timeoutMsg: `${key} の編集内容が反映されませんでした。`,
  });
  await waitForText('[data-testid="editor-save-status"]', "保存済み", 15_000);
  return target;
}

async function chooseScreenshotExportKind() {
  await $('[for="export-option-modpackFiles"]').click();
  await browser.waitUntil(async () => await $('#export-option-modpackFiles').isSelected(), {
    timeout: 5_000,
    interval: 100,
  });
}

async function selectOnlyScreenshotModpackUnit() {
  await $(".export-unit-selector").$("button=全解除").click();
  for (const label of await $$(".export-unit-selector label")) {
    if ((await label.getText()).includes("Modpack 固有")) {
      const checkbox = await label.$('input[type="checkbox"]');
      await label.click();
      assert.equal(await checkbox.isSelected(), true);
      return;
    }
  }
  assert.fail("書き出し対象のModpack unitがありません。");
}

async function restoreScreenshotDeployment(pathFragment) {
  const section = await $('[aria-labelledby="export-file-deployments-title"]');
  await section.waitForDisplayed({ timeout: 15_000 });
  for (const item of await section.$$("li")) {
    if ((await item.getText()).replaceAll("\\", "/").includes(pathFragment.replaceAll("\\", "/"))) {
      await item.$("button").click();
      await browser.waitUntil(async () => await browser.execute((fragment) => (
        ![...document.querySelectorAll('[aria-labelledby="export-file-deployments-title"] li')]
          .some((row) => row.textContent.replaceAll("\\", "/").includes(fragment))
      ), pathFragment.replaceAll("\\", "/")), {
        timeout: 20_000,
        interval: 100,
        timeoutMsg: `配置ファイル ${pathFragment} を復元できませんでした。`,
      });
      return;
    }
  }
  assert.fail(`復元対象の配置ファイル「${pathFragment}」がありません。`);
}

async function waitForScreenshotExportCompletion() {
  await browser.waitUntil(async () => (
    await $(".export-completion h1").isDisplayed()
    || await $(".export-screen [role='alert']").isDisplayed()
  ), { timeout: 90_000, interval: 100, timeoutMsg: "書き出し結果が表示されませんでした。" });
  const error = await $(".export-screen [role='alert']");
  if (await error.isDisplayed()) assert.fail(`書き出しエラー: ${await error.getText()}`);
  await waitForText(".export-completion h1", "書き出しました", 10_000);
}

async function captureGeneratedFeatureScreens(theme, fixtures) {
  await scanGeneratedInstance(fixtures.singleInstance);
  const modpackRow = await findScanRow("Modpack 固有（クエスト・KubeJS）");
  assert.match(await modpackRow.getText(), /3 件/);
  assert.match(await modpackRow.getText(), /クエスト 2 件 · KubeJS 1 件/);
  await capture(theme, "scan-modpack-files");
  await returnHome();

  await scanGeneratedInstance(fixtures.splitInstance);
  const splitRow = await findScanRow("Modpack 固有（クエスト・KubeJS）");
  assert.match(await splitRow.getText(), /1 件/);
  assert.match(await splitRow.getText(), /クエスト 1 件 · KubeJS 0 件/);
  await capture(theme, "scan-modpack-split");
  await returnHome();

  await scanGeneratedInstance(fixtures.legacyInstance);
  const legacyRow = await findScanRow("Modpack 固有（クエスト・KubeJS）");
  assert.match(await legacyRow.getText(), /クエスト旧形式/);
  assert.match(await legacyRow.getText(), /この形式のクエストは翻訳できません/);
  assert.equal(await legacyRow.$('input[type="checkbox"]').isEnabled(), false);
  await capture(theme, "scan-modpack-legacy-unsupported");
  await returnHome();

  await scanGeneratedInstance(fixtures.singleInstance);
  for (const row of await $$(".scan-screen__row")) {
    const checkbox = await row.$('input[type="checkbox"]');
    if (await checkbox.isEnabled() && await checkbox.isSelected()) await row.$("label").click();
  }
  await selectScanRow("Fixture Body Guide", true);
  await selectScanRow("Modpack 固有（クエスト・KubeJS）", true);
  const createProject = await $(".scan-screen__actions button");
  await browser.waitUntil(async () => (await createProject.getText()).includes("2 件"), {
    timeout: 5_000,
    interval: 100,
    timeoutMsg: "Patchouli本文とModpackの2件が選択されませんでした。",
  });
  await createProject.click();
  await waitForText(".review-screen h1", "Mod情報を確認", 60_000);
  const reviewUnits = await $(".review-mod-list").getText();
  assert.match(reviewUnits, /Fixture Body Guide/);
  assert.match(reviewUnits, /Modpack 固有/);
  await capture(theme, "review-modpack-namespaces");
  await advanceReviewToEditor();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });

  const patchouliKey = "body_guide › entries/start.json › pages[0].text";
  await selectScreenshotModTab("Fixture Body Guide");
  const patchouliTarget = await $(`[data-testid="translation-target-${patchouliKey}"]`);
  await patchouliTarget.waitForDisplayed({ timeout: 30_000 });
  await patchouliTarget.click();
  await $(".editor-inspector__scroll").waitForDisplayed({ timeout: 5_000 });
  await browser.execute(() => {
    const inspector = document.querySelector(".editor-inspector__scroll");
    if (inspector) inspector.scrollTop = inspector.scrollHeight;
  });
  const patchouliInspector = await $(".editor-inspector").getText();
  assert.match(patchouliInspector, /\$\(item\)/);
  await capture(theme, "editor-patchouli-body-selected");

  const ftbKey = "quest.fixture › description";
  await selectScreenshotModTab("Modpack 固有");
  const ftbTarget = await $(`[data-testid="translation-target-${ftbKey}"]`);
  await ftbTarget.waitForDisplayed({ timeout: 30_000 });
  await ftbTarget.click();
  await capture(theme, "editor-ftb-selected");
  const ftbEntryId = await ftbTarget.getAttribute("data-entry-id");
  const ftbWarning = await $(`[role="row"][data-entry-id="${ftbEntryId}"] .editor-table__warning`);
  await editScreenshotTranslation(ftbKey, "書式を壊したクエストの説明");
  await browser.waitUntil(async () => await ftbWarning.isExisting(), {
    timeout: 10_000,
    interval: 100,
    timeoutMsg: "FTB Questsの書式警告が表示されませんでした。",
  });
  await capture(theme, "editor-ftb-format-warning");

  await $('[data-testid="editor-export"]').click();
  await $(".export-screen").waitForDisplayed({ timeout: 15_000 });
  await chooseScreenshotExportKind();
  await selectOnlyScreenshotModpackUnit();
  await $('[name="modpack-files-destination"][value="folder"]').click();
  const originalWindowSize = await browser.getWindowSize();
  await browser.setWindowSize(originalWindowSize.width, Math.max(originalWindowSize.height, 1000));
  await capture(theme, "export-modpack-destinations");
  await browser.setWindowSize(originalWindowSize.width, originalWindowSize.height);

  const placeholderDetails = await $$(".export-validation__details");
  let placeholderDisclosure;
  for (const details of placeholderDetails) {
    if ((await details.$("summary").getText()).includes("書式エラー")) {
      placeholderDisclosure = details;
      break;
    }
  }
  assert.ok(placeholderDisclosure, "書式警告の一覧がありません。");
  await placeholderDisclosure.$("summary").click();
  await browser.waitUntil(async () => await placeholderDisclosure.getAttribute("open") !== null, {
    timeout: 5_000,
    interval: 100,
    timeoutMsg: "FTB Questsの書式警告一覧を開けませんでした。",
  });
  const validationList = await $(".export-validation__list");
  await validationList.waitForDisplayed({ timeout: 10_000 });
  assert.match(await validationList.getText(), /\.snbt/, "書式警告のラベルにFTBファイル名がありません。");
  await browser.execute(() => document.querySelector(".export-validation__details")
    ?.scrollIntoView({ block: "center", behavior: "instant" }));
  await capture(theme, "export-ftb-format-warnings");

  await $("button=戻る").click();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 15_000 });
  await selectScreenshotModTab("Modpack 固有");
  await editScreenshotTranslation(ftbKey, "&a遺物を探す {@pagebreak} {image:fixture:map}");
  await $('[data-testid="editor-export"]').click();
  await $(".export-screen").waitForDisplayed({ timeout: 15_000 });
  await chooseScreenshotExportKind();
  await selectOnlyScreenshotModpackUnit();
  await $('[name="modpack-files-destination"][value="instance"]').click();
  await browser.execute(() => { window.confirm = () => true; });
  await $('[data-testid="export-start"]').click();
  await waitForScreenshotExportCompletion();
  await capture(theme, "export-modpack-direct-complete");

  await $("button=別の形式で書き出す").click();
  const deployments = await $('[aria-labelledby="export-file-deployments-title"]');
  await deployments.waitForDisplayed({ timeout: 15_000 });
  await browser.execute(() => document.querySelector('[aria-labelledby="export-file-deployments-title"]')
    ?.scrollIntoView({ block: "center", behavior: "instant" }));
  await capture(theme, "export-modpack-restore");
  await restoreScreenshotDeployment("ja_jp.snbt");
  await browser.execute(() => document.querySelector('[aria-labelledby="export-file-deployments-title"]')
    ?.scrollIntoView({ block: "center", behavior: "instant" }));
  await capture(theme, "export-modpack-restore-result");
  await restoreScreenshotDeployment("ja_jp.json");

  const ftbPath = path.join(fixtures.singleInstance, "config/ftbquests/quests/lang/ja_jp.snbt");
  await $('[data-testid="export-start"]').click();
  await waitForScreenshotExportCompletion();
  const deployedFtbBytes = await readFile(ftbPath);
  await writeFile(ftbPath, "externally edited after deployment\n", "utf8");
  await $("button=別の形式で書き出す").click();
  await $('[data-testid="export-start"]').click();
  await browser.waitUntil(async () => await $(".export-screen [role='alert']").isDisplayed(), {
    timeout: 30_000,
    interval: 100,
    timeoutMsg: "外部変更されたFTBファイルへの配置を拒否しませんでした。",
  });
  const deployError = await $(".export-screen [role='alert']").getText();
  assert.match(deployError, /ja_jp\.snbt/);
  await browser.execute(() => document.querySelector(".export-screen [role='alert']")
    ?.scrollIntoView({ block: "center", behavior: "instant" }));
  await capture(theme, "export-modpack-external-change-error");
  await writeFile(ftbPath, deployedFtbBytes);
  await restoreScreenshotDeployment("ja_jp.snbt");
  await restoreScreenshotDeployment("ja_jp.json");
  await returnHome();
}

async function captureShaderpackScreens(theme, fixtures) {
  await scanGeneratedInstance(fixtures.singleInstance);
  await findScanRow("Aurora.zip");
  await capture(theme, "scan-shaderpacks");
  for (const row of await $$(".scan-screen__row")) {
    const checkbox = await row.$('input[type="checkbox"]');
    if (await checkbox.isEnabled() && await checkbox.isSelected()) await row.$("label").click();
  }
  await selectScanRow("Aurora.zip", true);
  await selectScanRow("Fixture i18n Guide", true);
  await $(".scan-screen__actions button").click();
  await $(".review-screen h1").waitForDisplayed({ timeout: 60_000 });
  await capture(theme, "review-shaderpack");
  await advanceReviewToEditor();
  await selectScreenshotModTab("Aurora.zip");
  await $('[data-testid="translation-target-option.BLOOM"]').waitForDisplayed({ timeout: 30_000 });
  await capture(theme, "editor-shaderpack");
  // Tauri のネイティブイベントを WebDriver から再現する。
  await browser.execute((event, payload) => {
    const listeners = window.__internal_unstable_listeners_object_id__?.[event];
    if (!listeners || Object.getOwnPropertyNames(listeners).length === 0) throw new Error(`イベント購読がありません: ${event}`);
    window.__internal_unstable_listeners_function_id__({ event, payload }, Object.getOwnPropertyNames(listeners));
  }, "tauri://drag-enter", { paths: ["Aurora.zip"], position: { x: 100, y: 100 } });
  await $(".editor-drop-overlay").waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "editor-drop-overlay");
  await browser.execute(() => {
    const event = "tauri://drag-leave";
    const listeners = window.__internal_unstable_listeners_object_id__?.[event];
    if (!listeners || Object.getOwnPropertyNames(listeners).length === 0) throw new Error(`イベント購読がありません: ${event}`);
    window.__internal_unstable_listeners_function_id__({ event, payload: null }, Object.getOwnPropertyNames(listeners));
  });
  await $(".editor-drop-overlay").waitForDisplayed({ reverse: true, timeout: 5_000 });
  await $('[data-testid="editor-add-mod"]').click();
  await $("button=インスタンスから選ぶ").click();
  await $(".add-units-dialog__list").waitForDisplayed({ timeout: 60_000 });
  await capture(theme, "editor-add-units-dialog");
  await browser.keys("Escape");
  await selectScreenshotModTab("Fixture i18n Guide");
  await editScreenshotTranslation("block.fixture.relic", "遺物 %d");
  await $('[data-testid="editor-export"]').click();
  await $(".export-screen").waitForDisplayed({ timeout: 15_000 });
  await $('[for="export-option-shaderpackZip"]').click();
  await capture(theme, "export-shaderpack");
  await $('[for="export-option-resourcePack"]').click();
  await $('[data-testid="export-start"]').click();
  await $('[role="dialog"]').waitForDisplayed({ timeout: 30_000 });
  await capture(theme, "export-format-error-blocked");
  await $("button=このまま書き出す").click();
  await waitForScreenshotExportCompletion();
  await capture(theme, "export-complete-with-format-errors");
  await returnHome();
}

describe("全画面スクリーンショット", () => {
  it("各画面をライトとダークで保存する", async function () {
    this.timeout(900_000);
    const fixturePath = process.env.YAKUCRAFT_E2E_LARGE_FIXTURE;
    const darkFixturePath = process.env.YAKUCRAFT_E2E_DARK_FIXTURE;
    const exportPath = process.env.YAKUCRAFT_E2E_EXPORT;
    assert.ok(fixturePath, "fixture JAR のパスがありません。");
    assert.ok(darkFixturePath, "ダーク用fixture JAR のパスがありません。");
    assert.ok(exportPath, "書き出し先のパスがありません。");
    const variantFixturePath = path.join(path.dirname(fixturePath), "variant-selector-fixture.jar");
    const multiModInstancePath = path.join(path.dirname(fixturePath), "multi-mod-instance");
    const resourcePackPath = path.join(path.dirname(exportPath), "resource-pack-screenshot.zip");
    const lightFeatureFixtures = await generateFeatureFixtures(path.join(path.dirname(fixturePath), "feature-light"));
    const darkFeatureFixtures = await generateFeatureFixtures(path.join(path.dirname(fixturePath), "feature-dark"));
    const modelMock = await startScreenshotModelServer();
    try {
      await createVariantFixture(variantFixturePath);
      await createMultiModFixture(multiModInstancePath);
      await $('[data-testid="home-open-jar"]').waitForClickable({ timeout: 30_000 });

      await browser.execute((paths) => {
        window.__YAKUCRAFT_E2E__ = paths;
      }, { jarPath: fixturePath, exportPath: resourcePackPath });

      await captureSettings("light", modelMock.url, modelMock.requests);
      await $('.app-brand[aria-label="ホームへ戻る"]').click();
      await $('[data-testid="home-open-jar"]').waitForDisplayed({ timeout: 10_000 });
      await captureProjectFlow("light", fixturePath);
      await captureVariantSelector("light", variantFixturePath);
      await captureMultiModFlow("light", multiModInstancePath);
      await returnHome();
      await captureGeneratedFeatureScreens("light", lightFeatureFixtures);
      await captureShaderpackScreens("light", lightFeatureFixtures);

      await captureSettings("dark", modelMock.url, modelMock.requests);
      await $('.app-brand[aria-label="ホームへ戻る"]').click();
      await $('[data-testid="home-open-jar"]').waitForDisplayed({ timeout: 10_000 });
      await captureProjectFlow("dark", darkFixturePath);
      await captureVariantSelector("dark", variantFixturePath);
      await captureMultiModFlow("dark", multiModInstancePath);
      await returnHome();
      await captureGeneratedFeatureScreens("dark", darkFeatureFixtures);
      await captureShaderpackScreens("dark", darkFeatureFixtures);
    } finally {
      modelMock.server.closeAllConnections?.();
      await new Promise((resolve) => modelMock.server.close(resolve));
    }
  });
});
