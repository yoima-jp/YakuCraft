import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { inflateRawSync } from "node:zlib";

const editedTranslation = "E2E merged translation";
const firstKey = "block.shared.alpha";
const secondKey = "block.shared.beta";
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

function makeJar(entries) {
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
    local.writeUInt16LE(0, 8);
    local.writeUInt16LE(0, 10);
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
    central.writeUInt16LE(0, 10);
    central.writeUInt16LE(0, 12);
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

function fixtureJar(modId, name, namespace, sourceEntries, japaneseEntries) {
  const json = (value) => Buffer.from(JSON.stringify(value), "utf8");
  const entries = [
    ["fabric.mod.json", json({
      schemaVersion: 1,
      id: modId,
      name,
      version: "1.0.0",
      depends: { minecraft: "1.21.1" },
    })],
    ["assets/" + namespace + "/lang/en_us.json", json(sourceEntries)],
  ];
  if (japaneseEntries) entries.push(["assets/" + namespace + "/lang/ja_jp.json", json(japaneseEntries)]);
  return makeJar(entries);
}

function readZipEntries(bytes) {
  let endOffset = -1;
  for (let offset = bytes.length - 22; offset >= Math.max(0, bytes.length - 65_557); offset -= 1) {
    if (bytes.readUInt32LE(offset) === 0x06054b50) {
      endOffset = offset;
      break;
    }
  }
  assert.notEqual(endOffset, -1, "ZIP の終了レコードがありません。");

  const entryCount = bytes.readUInt16LE(endOffset + 10);
  let centralOffset = bytes.readUInt32LE(endOffset + 16);
  const entries = new Map();
  for (let index = 0; index < entryCount; index += 1) {
    assert.equal(bytes.readUInt32LE(centralOffset), 0x02014b50, "ZIP の中央ディレクトリが不正です。");
    const method = bytes.readUInt16LE(centralOffset + 10);
    const compressedSize = bytes.readUInt32LE(centralOffset + 20);
    const nameLength = bytes.readUInt16LE(centralOffset + 28);
    const extraLength = bytes.readUInt16LE(centralOffset + 30);
    const commentLength = bytes.readUInt16LE(centralOffset + 32);
    const localOffset = bytes.readUInt32LE(centralOffset + 42);
    const name = bytes.toString("utf8", centralOffset + 46, centralOffset + 46 + nameLength);
    const localNameLength = bytes.readUInt16LE(localOffset + 26);
    const localExtraLength = bytes.readUInt16LE(localOffset + 28);
    const dataOffset = localOffset + 30 + localNameLength + localExtraLength;
    const compressed = bytes.subarray(dataOffset, dataOffset + compressedSize);
    if (method === 0) entries.set(name, Buffer.from(compressed));
    else if (method === 8) entries.set(name, inflateRawSync(compressed));
    else assert.fail("未対応のZIP圧縮方式です: " + method);
    centralOffset += 46 + nameLength + extraLength + commentLength;
  }
  return entries;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

async function waitForText(selector, expectedText, timeout = 30_000) {
  const element = await $(selector);
  await element.waitForDisplayed({ timeout });
  await browser.waitUntil(async () => (await element.getText()) === expectedText, {
    timeout,
    interval: 100,
    timeoutMsg: selector + " が「" + expectedText + "」になりませんでした。",
  });
  return element;
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
  assert.fail("スキャン結果に " + name + " がありません。\n" + rows.join("\n") + "\n" + screenText);
}

async function translationTargetText(key) {
  const selector = `[data-testid="translation-target-${key}"]`;
  return browser.execute((targetSelector) => document.querySelector(targetSelector)?.textContent ?? "", selector);
}

async function clickModTab(name) {
  const tabSelector = '[role="tablist"][aria-label="開いているmod"] [role="tab"]';
  let matchingTab = false;
  for (const tab of await $$(tabSelector)) {
    if (await browser.execute((element, expected) => element.textContent.includes(expected), tab, name)) matchingTab = true;
  }
  let selector = tabSelector;
  if (!matchingTab) {
    await $(".mod-tabs__all-button").click();
    await browser.waitUntil(async () => (await $(".mod-tabs__all-button").getAttribute("aria-expanded")) === "true", {
      timeout: 2_000,
      interval: 50,
      timeoutMsg: "すべてのmodの一覧を開けませんでした。",
    });
    await $(".mod-tabs__panel").waitForDisplayed({ timeout: 10_000 });
    selector = ".mod-tabs__open-unit";
  }
  await browser.execute((targetSelector, targetName) => {
    const target = [...document.querySelectorAll(targetSelector)]
      .find((element) => element.textContent.includes(targetName));
    if (!target) throw new Error("mod tab not found: " + targetName);
    target.click();
  }, selector, name);
  await browser.waitUntil(async () => {
    const activeTab = await $(tabSelector + '[aria-selected="true"]');
    return browser.execute((element, expected) => element.textContent.includes(expected), activeTab, name);
  }, { timeout: 10_000, interval: 100 });
}

async function editTranslation(key, value) {
  const target = await $('[data-testid="translation-target-' + key + '"]');
  await target.waitForDisplayed({ timeout: 30_000 });
  await target.click();
  await browser.keys("Enter");
  const editor = await $('textarea[aria-label="' + key + ' の日本語訳"]');
  await editor.waitForDisplayed({ timeout: 5_000 });
  await browser.waitUntil(() => editor.isFocused(), { timeout: 5_000, interval: 50 });
  await browser.keys(["Control", "A"]);
  await browser.keys(value);
  await browser.keys("Enter");
  await browser.waitUntil(async () => (await translationTargetText(key)) === value, {
    timeout: 10_000,
    interval: 100,
    timeoutMsg: key + " の編集内容が表に反映されませんでした。",
  });
  await waitForText('[data-testid="editor-save-status"]', "保存中…", 10_000).catch(async () => {
    await $('[data-testid="editor-save-status"]').waitForDisplayed({ timeout: 5_000 });
  });
  await waitForText('[data-testid="editor-save-status"]', "保存済み", 15_000);
}

async function advanceReviewToEditor() {
  const next = await $('[data-testid="review-next"]');
  for (let step = 0; step < 12; step += 1) {
    await next.waitForClickable({ timeout: 60_000 });
    const label = (await next.getText()).trim();
    if (label === "翻訳を始める") {
      await next.click();
      await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 60_000 });
      return;
    }
    assert.equal(label, "次へ", "Reviewを進められません: " + label);
    const heading = await $(".review-screen h1").getText();
    await next.click();
    await browser.waitUntil(async () => {
      const currentHeading = await $(".review-screen h1");
      return !await currentHeading.isExisting() || (await currentHeading.getText()) !== heading;
    }, { timeout: 15_000, interval: 100, timeoutMsg: "Reviewの次の手順へ進みませんでした。" });
  }
  assert.fail("Reviewの全手順を終えられませんでした。");
}

describe("インスタンスフォルダからリソースパックを書き出す", () => {
  it("フォルダ選択、mod切替、同一namespaceのマージ、直接配置、JAR復元を確認できる", async () => {
    const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "yakucraft-modpack-flow-"));
    const instancePath = path.join(temporaryRoot, "instance");
    const modsPath = path.join(instancePath, "mods");
    const untranslatedJarPath = path.join(modsPath, "01-untranslated.jar");
    const partialJarPath = path.join(modsPath, "02-partial.jar");
    const completeJarPath = path.join(modsPath, "03-complete.jar");
    const packLanguagePath = "assets/shared/lang/ja_jp.json";
    const untranslatedJar = fixtureJar(
      "untranslated",
      "Untranslated Mod",
      "shared",
      { "block.shared.alpha": "Alpha", "item.shared.alpha": "Alpha Item" },
      null,
    );
    const untranslatedHash = sha256(untranslatedJar);

    try {
      await mkdir(modsPath, { recursive: true });
      await writeFile(path.join(instancePath, "options.txt"), "version:1\n", "utf8");
      await writeFile(untranslatedJarPath, untranslatedJar);
      await writeFile(partialJarPath, fixtureJar(
        "partial",
        "Partial Mod",
        "shared",
        { "block.shared.beta": "Beta", "item.shared.beta": "Beta Item" },
        { "block.shared.beta": "既存訳" },
      ));
      await writeFile(completeJarPath, fixtureJar(
        "complete",
        "Complete Mod",
        "complete",
        { "block.complete.machine": "Complete" },
        { "block.complete.machine": "翻訳済み" },
      ));

      await browser.execute((directoryPath) => {
        window.__YAKUCRAFT_E2E__ = { directoryPath };
      }, instancePath);
      await $('[data-testid="home-open-mods-folder"]').click();
      const untranslatedRow = await scanRowFor("Untranslated Mod");
      const partialRow = await scanRowFor("Partial Mod");
      const completeRow = await scanRowFor("Complete Mod");
      assert.match(await untranslatedRow.getText(), /未翻訳/);
      assert.match(await partialRow.getText(), /部分翻訳/);
      assert.match(await completeRow.getText(), /翻訳済み/);
      assert.equal(await untranslatedRow.$('input[type="checkbox"]').isSelected(), true);
      assert.equal(await partialRow.$('input[type="checkbox"]').isSelected(), true);
      assert.equal(await completeRow.$('input[type="checkbox"]').isSelected(), false);

      const createProject = await $(".scan-screen__actions button");
      await createProject.waitForClickable({ timeout: 10_000 });
      assert.equal((await createProject.getText()).trim(), "2 件でプロジェクトを作成");
      await createProject.click();
      await $(".review-screen h1").waitForDisplayed({ timeout: 60_000 });
      await advanceReviewToEditor();

      const tabList = await $('[role="tablist"][aria-label="開いているmod"]');
      await tabList.waitForDisplayed({ timeout: 30_000 });
      assert.equal((await tabList.$$('[role="tab"]')).length, 2, "初回は選択したすべてのmodがタブで開くはずです。");
      await clickModTab("Untranslated Mod");
      await $('[data-testid="translation-target-block.shared.alpha"]').waitForDisplayed({ timeout: 30_000 });
      await clickModTab("Partial Mod");
      await browser.waitUntil(async () => {
        const activeTab = await $('[role="tablist"][aria-label="開いているmod"] [role="tab"][aria-selected="true"]');
        return browser.execute((element) => element.textContent.includes("Partial Mod"), activeTab);
      }, { timeout: 10_000, interval: 100 });
      assert.equal((await tabList.$$('[role="tab"]')).length, 2, "切り替え後に2つのmodタブが表示されるはずです。");
      await $('[data-testid="translation-target-block.shared.beta"]').waitForDisplayed({ timeout: 30_000 });
      assert.equal(await $('[data-testid="translation-target-block.shared.alpha"]').isExisting(), false);
      await clickModTab("Untranslated Mod");
      await editTranslation(firstKey, editedTranslation);

      await $('[data-testid="editor-export"]').click();
      await $(".export-screen").waitForDisplayed({ timeout: 30_000 });
      await $('[name="resource-pack-destination"][value="instance"]').click();
      const versionSelect = await $('[aria-label="Minecraft バージョン"]');
      await versionSelect.waitForDisplayed({ timeout: 30_000 });
      if ((await versionSelect.getText()).trim() !== "1.21.1") {
        await versionSelect.click();
        await $('[role="option"][data-value="1.21.1"]').click();
      }
      const startResourcePack = await $('[data-testid="export-start"]');
      await startResourcePack.waitForClickable({ timeout: 30_000 });
      await startResourcePack.click();
      await browser.waitUntil(async () =>
        await $(".export-completion h1").isDisplayed()
        || await $(".export-screen [role=\"alert\"]").isDisplayed(),
      { timeout: 60_000, interval: 100, timeoutMsg: "書き出し結果が表示されませんでした。" });
      const exportError = await $(".export-screen [role=\"alert\"]");
      if (await exportError.isDisplayed()) assert.fail("リソースパック書き出しエラー: " + await exportError.getText());
      await waitForText(".export-completion h1", "書き出しました", 10_000);
      assert.equal((await $(".export-completion").getText()).includes("インスタンスに直接配置"), true);

      const resourcepacksPath = path.join(instancePath, "resourcepacks");
      const packNames = (await readdir(resourcepacksPath)).filter((name) => name.toLowerCase().endsWith(".zip"));
      assert.equal(packNames.length, 1, "インスタンスの resourcepacks に ZIP が1つ作られるはずです。");
      const packEntries = readZipEntries(await readFile(path.join(resourcepacksPath, packNames[0])));
      assert.equal(packEntries.has("pack.mcmeta"), true);
      assert.equal(packEntries.has(packLanguagePath), true);
      const mergedLanguage = JSON.parse(packEntries.get(packLanguagePath).toString("utf8"));
      assert.equal(mergedLanguage[firstKey], editedTranslation);
      assert.equal(mergedLanguage[secondKey], "既存訳");

      await $('button=別の形式で書き出す').click();
      await $(".export-screen").waitForDisplayed({ timeout: 30_000 });
      await $('[for="export-option-modifiedJar"]').click();
      for (const label of await $$(".export-unit-selector label")) {
        const checkbox = await label.$('input[type="checkbox"]');
        const labelText = await browser.execute((element) => element.textContent, label);
        const shouldSelect = labelText.includes("Untranslated Mod");
        if ((await checkbox.isSelected()) !== shouldSelect) await checkbox.click();
      }
      const inPlaceCheckbox = await $('.export-screen__advanced-check input[type="checkbox"]');
      await inPlaceCheckbox.click();
      await $('[role="dialog"]').waitForDisplayed({ timeout: 30_000 });
      await $("button=置き換え").click();
      assert.equal(await inPlaceCheckbox.isSelected(), true);
      const startJarExport = await $('[data-testid="export-start"]');
      await startJarExport.waitForClickable({ timeout: 30_000 });
      await startJarExport.click();
      await waitForText(".export-completion h1", "書き出しました", 60_000);
      const overwrittenJar = readZipEntries(await readFile(untranslatedJarPath));
      assert.equal(overwrittenJar.has(packLanguagePath), true, "上書き後のJARに日本語ファイルがありません。");
      assert.equal(JSON.parse(overwrittenJar.get(packLanguagePath).toString("utf8"))[firstKey], editedTranslation);
      assert.notEqual(sha256(await readFile(untranslatedJarPath)), untranslatedHash);

      await $('button=別の形式で書き出す').click();
      await $(".export-screen").waitForDisplayed({ timeout: 30_000 });
      await $(".export-replacements li").waitForDisplayed({ timeout: 30_000 });
      let restoreButton;
      for (const item of await $$(".export-replacements li")) {
        if ((await browser.execute((element) => element.textContent, item)).includes("Untranslated Mod")) {
          restoreButton = await item.$("button");
          break;
        }
      }
      assert.ok(restoreButton, "上書きしたmodの復元操作がありません。");
      await restoreButton.click();
      await browser.waitUntil(async () => sha256(await readFile(untranslatedJarPath)) === untranslatedHash, {
        timeout: 30_000,
        interval: 100,
        timeoutMsg: "JARを元に戻した後も SHA-256 が一致しません。",
      });
    } finally {
      await rm(temporaryRoot, { recursive: true, force: true });
    }
  });
});
