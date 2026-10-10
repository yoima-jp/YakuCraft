import assert from "node:assert/strict";
import { readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { deflateSync } from "node:zlib";

const translationKey = "block.sample.machine";

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

function pngChunk(type, data) {
  const name = Buffer.from(type, "ascii");
  const chunk = Buffer.alloc(12 + data.length);
  chunk.writeUInt32BE(data.length, 0);
  name.copy(chunk, 4);
  data.copy(chunk, 8);
  chunk.writeUInt32BE(crc32(Buffer.concat([name, data])), 8 + data.length);
  return chunk;
}

function makePng() {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(1, 0);
  header.writeUInt32BE(1, 4);
  header.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(Buffer.from([0, 40, 150, 70, 255]))),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
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

function makePreviewFixture() {
  const json = (value) => Buffer.from(JSON.stringify(value), "utf8");
  const modelFaces = Object.fromEntries(
    ["north", "south", "east", "west", "up", "down"].map((face) => [face, { texture: "#all" }]),
  );
  return makeJar([
    ["fabric.mod.json", json({ schemaVersion: 1, id: "sample", name: "Delete Fixture", version: "1.0.0" })],
    ["assets/sample/lang/en_us.json", json({ [translationKey]: "Machine" })],
    ["assets/sample/lang/ja_jp.json", json({ [translationKey]: "機械" })],
    ["assets/sample/blockstates/machine.json", json({ variants: { "": { model: "sample:block/machine" } } })],
    ["assets/sample/models/block/machine.json", json({
      textures: { all: "sample:block/machine" },
      elements: [{ from: [0, 0, 0], to: [16, 16, 16], faces: modelFaces }],
    })],
    ["assets/sample/textures/block/machine.png", makePng()],
  ]);
}

async function projectDirectories(projectsPath) {
  const entries = await readdir(projectsPath, { withFileTypes: true });
  return entries.filter((entry) => entry.isDirectory() && !entry.name.startsWith(".trash-")).map((entry) => entry.name);
}

async function trashDirectories(projectsPath) {
  const entries = await readdir(projectsPath, { withFileTypes: true });
  return entries.filter((entry) => entry.isDirectory() && entry.name.startsWith(".trash-")).map((entry) => entry.name);
}

describe("Recentから開いたプロジェクトを削除", () => {
  it("previewとthumbnailを読み込んでから3回連続で作成・削除できる", async () => {
    const originalFixture = process.env.YAKUCRAFT_E2E_FIXTURE;
    const dataDirectory = process.env.YAKUCRAFT_DATA_DIR;
    assert.ok(originalFixture, "fixture JAR のパスがありません。");
    assert.ok(dataDirectory, "E2E data directory のパスがありません。");

    const fixturePath = path.join(path.dirname(originalFixture), "project-delete-fixture.jar");
    const projectsPath = path.join(dataDirectory, "projects");
    await writeFile(fixturePath, makePreviewFixture());

    for (let cycle = 0; cycle < 3; cycle += 1) {
      const before = new Set(await projectDirectories(projectsPath));
      await browser.execute((jarPath) => {
        window.__YAKUCRAFT_E2E__ = { jarPath };
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
      const target = await $(`[data-testid="translation-target-${translationKey}"]`);
      await target.waitForDisplayed({ timeout: 30_000 });
      await target.click();

      await $('[aria-label="アセットプレビュー"]').waitForDisplayed({ timeout: 30_000 });
      await browser.waitUntil(async () => {
        const thumbnail = await $(`[data-testid="translation-thumbnail-${translationKey}"] img`);
        return thumbnail.isExisting() && thumbnail.isDisplayed();
      }, { timeout: 60_000, interval: 100, timeoutMsg: "サムネイルが表示されませんでした。" });

      const created = (await projectDirectories(projectsPath)).filter((id) => !before.has(id));
      assert.equal(created.length, 1, "作成したProject directoryを特定できません。");
      const projectId = created[0];

      await $('[aria-label="ホームへ戻る"]').click();
      await $(".home-screen").waitForDisplayed({ timeout: 15_000 });
      const recentCardSelector = "//li[contains(@class, 'recent-projects__item')][.//span[contains(@class, 'recent-projects__name') and normalize-space()='Delete Fixture']]";
      const recentCard = await $(recentCardSelector);
      await recentCard.waitForDisplayed({ timeout: 15_000 });
      if (cycle === 1) {
        await recentCard.click({ button: "right" });
        const contextMenu = await $("[role=\"menu\"]");
        await contextMenu.waitForDisplayed({ timeout: 5_000 });
        const menuItems = await contextMenu.$$("[role=\"menuitem\"]");
        let deleteMenuItem;
        for (const item of menuItems) {
          if ((await item.getText()).trim() === "削除") {
            deleteMenuItem = item;
            break;
          }
        }
        assert.ok(deleteMenuItem, "Recentのコンテキストメニューに削除がありません。");
        await deleteMenuItem.click();
      } else {
        const deleteButton = await recentCard.$('button[aria-label="Delete Fixtureを削除"]');
        assert.equal(await deleteButton.isExisting(), true, "Recentカードに削除ボタンがありません。");
        await deleteButton.click();
      }
      await $('[aria-modal="true"] .button--destructive').click();

      await browser.waitUntil(async () => {
        const ids = await projectDirectories(projectsPath);
        const trash = await trashDirectories(projectsPath);
        const recentCard = await $(recentCardSelector);
        return !ids.includes(projectId)
          && trash.length === 0
          && !(await recentCard.isExisting());
      }, { timeout: 30_000, interval: 100, timeoutMsg: "削除後もProject directoryまたはRecent行が残っています。" });
    }
  });
});
