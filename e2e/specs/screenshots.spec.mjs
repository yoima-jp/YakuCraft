import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

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

async function captureSettings(theme) {
  await $("nav.app-header__actions button").click();
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
}

// 標準fixtureにはブロック状態がないため、Variant Selectorの撮影用JARを一時生成する。
function createVariantFixture(outputPath) {
  const encodedPath = Buffer.from(outputPath, "utf8").toString("base64");
  const script = `
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new()
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()
$OutputEncoding = [System.Text.UTF8Encoding]::new()
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archivePath = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${encodedPath}'))
$archive = [System.IO.Compression.ZipFile]::Open($archivePath, [System.IO.Compression.ZipArchiveMode]::Create)
try {
  function Add-TextEntry($archive, $name, $content) {
    $entry = $archive.CreateEntry($name)
    $writer = [System.IO.StreamWriter]::new($entry.Open(), [System.Text.UTF8Encoding]::new($false))
    $writer.Write($content)
    $writer.Dispose()
  }
  Add-TextEntry $archive 'fabric.mod.json' '{"schemaVersion":1,"id":"sample","name":"Variant Fixture","version":"1.0"}'
  Add-TextEntry $archive 'assets/sample/lang/en_us.json' '{"block.sample.machine":"Machine"}'
  Add-TextEntry $archive 'assets/sample/lang/ja_jp.json' '{"block.sample.machine":"機械"}'
  Add-TextEntry $archive 'assets/sample/blockstates/machine.json' '{"variants":{"facing=north":{"model":"sample:block/machine"},"facing=east":{"model":"sample:block/machine"},"facing=south":{"model":"sample:block/machine"},"facing=west":{"model":"sample:block/machine"}}}'
  Add-TextEntry $archive 'assets/sample/models/block/machine.json' '{"parent":"minecraft:block/cube_all","textures":{"all":"sample:block/machine"}}'
  $texture = $archive.CreateEntry('assets/sample/textures/block/machine.png')
  $stream = $texture.Open()
  $bytes = [Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADUlEQVR4nGNgYGAAAAAEAAEnNCcKAAAAAElFTkSuQmCC')
  $stream.Write($bytes, 0, $bytes.Length)
  $stream.Dispose()
} finally {
  $archive.Dispose()
}
`;
  const powershell = process.env.SystemRoot
    ? path.join(process.env.SystemRoot, "System32", "WindowsPowerShell", "v1.0", "powershell.exe")
    : "powershell.exe";
  const result = spawnSync(powershell, [
    "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script,
  ], { encoding: "utf8", windowsHide: true });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Variant fixture を作れませんでした: ${result.stderr || result.stdout}`);
}

async function captureProjectFlow(theme, fixturePath) {
  await browser.execute((jarPath) => {
    window.__YAKUCRAFT_E2E__.jarPath = jarPath;
  }, fixturePath);
  await capture(theme, "home");
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
  await $('[role="menuitem"]').moveTo();
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
  await $(".settings-actions button").click();
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
  await browser.waitUntil(async () => (
    await $(tabSelector + '[aria-selected="true"]').getText()
  ).includes(name), { timeout: 10_000, interval: 100 });
}

async function captureMultiModFlow(theme, instancePath) {
  await setModTabPosition("top");
  await $('[aria-label="ホームへ戻る"]').click();
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
  await $('[role="menuitem"]').waitForDisplayed({ timeout: 5_000 });
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
  await $(".export-screen__actions button").click();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 10_000 });

  await $('[aria-label="ホームへ戻る"]').click();
  await $(".recent-projects__item").waitForDisplayed({ timeout: 10_000 });
  await capture(theme, "home-recent-projects");
  await $(".recent-projects__item").click({ button: "right" });
  await $('[role="menuitem"]').waitForDisplayed({ timeout: 5_000 });
  await capture(theme, "home-recent-project-context-menu");
  await browser.keys("Escape");
  await $(".recent-projects__open").click();
  await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
}

async function captureVariantSelector(theme, fixturePath) {
  await browser.execute((jarPath) => {
    window.__YAKUCRAFT_E2E__ = { ...window.__YAKUCRAFT_E2E__, jarPath };
  }, fixturePath);
  await $('[aria-label="ホームへ戻る"]').click();
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

describe("全画面スクリーンショット", () => {
  it("各画面をライトとダークで保存する", async () => {
    const fixturePath = process.env.YAKUCRAFT_E2E_LARGE_FIXTURE;
    const darkFixturePath = process.env.YAKUCRAFT_E2E_DARK_FIXTURE;
    const exportPath = process.env.YAKUCRAFT_E2E_EXPORT;
    assert.ok(fixturePath, "fixture JAR のパスがありません。");
    assert.ok(darkFixturePath, "ダーク用fixture JAR のパスがありません。");
    assert.ok(exportPath, "書き出し先のパスがありません。");
    const variantFixturePath = path.join(path.dirname(fixturePath), "variant-selector-fixture.jar");
    const multiModInstancePath = path.join(path.dirname(fixturePath), "multi-mod-instance");
    const resourcePackPath = path.join(path.dirname(exportPath), "resource-pack-screenshot.zip");
    createVariantFixture(variantFixturePath);
    await createMultiModFixture(multiModInstancePath);
    await $('[data-testid="home-open-jar"]').waitForClickable({ timeout: 30_000 });

    await browser.execute((paths) => {
      window.__YAKUCRAFT_E2E__ = paths;
    }, { jarPath: fixturePath, exportPath: resourcePackPath });

    await captureSettings("light");
    await $('[aria-label="ホームへ戻る"]').click();
    await $('[data-testid="home-open-jar"]').waitForDisplayed({ timeout: 10_000 });
    await captureProjectFlow("light", fixturePath);
    await captureVariantSelector("light", variantFixturePath);
    await captureMultiModFlow("light", multiModInstancePath);

    await $('[aria-label="ホームへ戻る"]').click();
    await $('[data-testid="home-open-jar"]').waitForDisplayed({ timeout: 10_000 });
    await captureSettings("dark");
    await $('[aria-label="ホームへ戻る"]').click();
    await $('[data-testid="home-open-jar"]').waitForDisplayed({ timeout: 10_000 });
    await captureProjectFlow("dark", darkFixturePath);
    await captureVariantSelector("dark", variantFixturePath);
    await captureMultiModFlow("dark", multiModInstancePath);
  });
});
