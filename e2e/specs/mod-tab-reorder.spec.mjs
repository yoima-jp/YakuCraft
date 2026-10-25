import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

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

function makeJar(modId, name, namespace) {
  const entries = [
    ["fabric.mod.json", Buffer.from(JSON.stringify({
      schemaVersion: 1,
      id: modId,
      name,
      version: "1.0.0",
      depends: { minecraft: "1.21.1" },
    }), "utf8")],
    [`assets/${namespace}/lang/en_us.json`, Buffer.from(JSON.stringify({
      [`block.${namespace}.machine`]: `${name} machine`,
    }), "utf8")],
  ];
  const localRecords = [];
  const centralRecords = [];
  let offset = 0;
  for (const [filePath, data] of entries) {
    const fileName = Buffer.from(filePath, "utf8");
    const checksum = crc32(data);
    const local = Buffer.alloc(30 + fileName.length);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6);
    local.writeUInt16LE(33, 12);
    local.writeUInt32LE(checksum, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(fileName.length, 26);
    fileName.copy(local, 30);
    localRecords.push(local, data);

    const central = Buffer.alloc(46 + fileName.length);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0x0800, 8);
    central.writeUInt16LE(33, 14);
    central.writeUInt32LE(checksum, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(fileName.length, 28);
    central.writeUInt32LE(offset, 42);
    fileName.copy(central, 46);
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

async function scanRowFor(name) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    for (const row of await $$(".scan-screen__row")) {
      if (await browser.execute((element, expected) => element.textContent.includes(expected), row, name)) return row;
    }
    await browser.pause(100);
  }
  const rows = [];
  for (const row of await $$(".scan-screen__row")) {
    rows.push(await browser.execute((element) => element.outerHTML, row));
  }
  const screenText = await $(".scan-screen").getText().catch(() => "スキャン画面がありません。");
  assert.fail(`スキャン結果に ${name} がありません。\n${rows.join("\n")}\n${screenText}`);
}

async function completeReview() {
  const editor = await $('[aria-label="翻訳エディター"]');
  for (let step = 0; step < 6; step += 1) {
    const heading = await $(".review-screen h1");
    await browser.waitUntil(async () => (await editor.isExisting()) || (await heading.isExisting()), {
      timeout: 60_000,
      interval: 100,
    });
    if (await editor.isExisting()) return;
    const previous = await heading.getText();
    await $('[data-testid="review-next"]').click();
    await browser.waitUntil(async () =>
      (await editor.isExisting()) || (await $(".review-screen h1").getText().catch(() => "")) !== previous,
    { timeout: 30_000, interval: 100 });
  }
}

async function tabOrder() {
  return browser.execute(() => [...document.querySelectorAll(
    '[role="tablist"][aria-label="開いているmod"] [role="tab"]',
  )].map((tab) => tab.getAttribute("data-mod-unit-id")));
}

describe("modタブの並べ替え", () => {
  it("ドラッグした順序をプロジェクトを開き直した後も保つ", async () => {
    const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "yakucraft-mod-tab-reorder-"));
    const modsPath = path.join(temporaryRoot, "mods");

    try {
      await mkdir(modsPath, { recursive: true });
      // The shared analysis fixture exposes one unit; independent jars ensure this flow has two tabs to reorder.
      await writeFile(path.join(modsPath, "alpha.jar"), makeJar("reorder_alpha", "Reorder Alpha Mod", "reorder_alpha"));
      await writeFile(path.join(modsPath, "beta.jar"), makeJar("reorder_beta", "Reorder Beta Mod", "reorder_beta"));

      await browser.execute((directoryPath) => {
        window.__YAKUCRAFT_E2E__ = { directoryPath };
      }, modsPath);
      await $('[data-testid="home-open-mods-folder"]').waitForClickable({ timeout: 30_000 });
      await $('[data-testid="home-open-mods-folder"]').click();
      const alphaRow = await scanRowFor("Reorder Alpha Mod");
      const betaRow = await scanRowFor("Reorder Beta Mod");
      assert.equal(await alphaRow.$('input[type="checkbox"]').isSelected(), true);
      assert.equal(await betaRow.$('input[type="checkbox"]').isSelected(), true);
      const createProject = await $(".scan-screen__actions button");
      await createProject.waitForClickable({ timeout: 10_000 });
      await createProject.click();
      await $(".review-screen h1").waitForDisplayed({ timeout: 60_000 });
      await completeReview();

      const tabListSelector = '[role="tablist"][aria-label="開いているmod"]';
      await $(tabListSelector).waitForDisplayed({ timeout: 60_000 });
      await browser.waitUntil(async () => (await tabOrder()).length >= 2, {
        timeout: 30_000,
        interval: 100,
        timeoutMsg: "並べ替え用のmodタブが2つ読み込まれませんでした。",
      });

      const tabs = await browser.execute(() => [...document.querySelectorAll(
        '[role="tablist"][aria-label="開いているmod"] [role="tab"]',
      )].slice(0, 2).map((tab) => {
        const bounds = tab.closest(".mod-tabs__item").getBoundingClientRect();
        return {
          id: tab.getAttribute("data-mod-unit-id"),
          x: bounds.x,
          y: bounds.y,
          width: bounds.width,
          height: bounds.height,
        };
      }));
      assert.equal(tabs.length, 2);
      const before = await tabOrder();
      const activeBefore = await browser.execute(() => document.querySelector(
        '[role="tablist"][aria-label="開いているmod"] [role="tab"][aria-selected="true"]',
      )?.getAttribute("data-mod-unit-id"));

      await browser.performActions([{
        type: "pointer",
        id: "mod-tab-drag",
        parameters: { pointerType: "mouse" },
        actions: [
          { type: "pointerMove", duration: 0, x: Math.round(tabs[0].x + tabs[0].width / 2), y: Math.round(tabs[0].y + tabs[0].height / 2) },
          { type: "pointerDown", button: 0 },
          { type: "pause", duration: 100 },
          { type: "pointerMove", duration: 250, x: Math.round(tabs[1].x + tabs[1].width * 0.8), y: Math.round(tabs[1].y + tabs[1].height / 2) },
          { type: "pointerUp", button: 0 },
        ],
      }]);
      await browser.releaseActions();

      const expected = [...before];
      [expected[0], expected[1]] = [expected[1], expected[0]];
      await browser.waitUntil(async () => JSON.stringify(await tabOrder()) === JSON.stringify(expected), {
        timeout: 10_000,
        interval: 100,
        timeoutMsg: "ドラッグ後のタブ順が変わりませんでした。",
      });
      assert.equal(
        await browser.execute(() => document.querySelector(
        '[role="tablist"][aria-label="開いているmod"] [role="tab"][aria-selected="true"]',
        )?.getAttribute("data-mod-unit-id")),
        activeBefore,
        "ドロップでアクティブタブが切り替わりました。",
      );

      await browser.keys(["Control", "o"]);
      await $(".home-screen").waitForDisplayed({ timeout: 10_000 });
      const recentProject = await $(
        '//button[contains(@class, "recent-projects__open")][.//span[contains(@class, "recent-projects__name") and normalize-space()="2 個の mod"]]',
      );
      await recentProject.waitForDisplayed({ timeout: 10_000 });
      await recentProject.waitForClickable({ timeout: 30_000 });
      await recentProject.click();
      await $('[aria-label="翻訳エディター"]').waitForDisplayed({ timeout: 30_000 });
      await $(tabListSelector).waitForDisplayed({ timeout: 30_000 });
      await browser.waitUntil(async () => JSON.stringify(await tabOrder()) === JSON.stringify(expected), {
        timeout: 10_000,
        interval: 100,
        timeoutMsg: "プロジェクトを開き直すと並べ替えたタブ順が復元されませんでした。",
      });
    } finally {
      await rm(temporaryRoot, { recursive: true, force: true });
    }
  });
});
