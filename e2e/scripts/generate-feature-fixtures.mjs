import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { inflateRawSync } from "node:zlib";
import { fileURLToPath } from "node:url";

const crcTable = Uint32Array.from({ length: 256 }, (_, value) => {
  let crc = value;
  for (let bit = 0; bit < 8; bit += 1) crc = (crc & 1) ? (0xedb88320 ^ (crc >>> 1)) : (crc >>> 1);
  return crc >>> 0;
});

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function makeZip(entries) {
  const localRecords = [];
  const centralRecords = [];
  let offset = 0;
  for (const [filePath, contents] of entries) {
    const name = Buffer.from(filePath, "utf8");
    const data = Buffer.isBuffer(contents) ? contents : Buffer.from(contents, "utf8");
    const checksum = crc32(data);
    const local = Buffer.alloc(30 + name.length);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6);
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

export function readZipEntries(bytes) {
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
    else assert.fail(`未対応のZIP圧縮方式です: ${method}`);
    centralOffset += 46 + nameLength + extraLength + commentLength;
  }
  return entries;
}

function json(value) {
  return JSON.stringify(value, null, 2) + "\n";
}

function fabricMetadata(id, name) {
  return json({
    schemaVersion: 1,
    id,
    name,
    version: "1.0.0",
    depends: { minecraft: "~1.20.1" },
  });
}

function patchouliJar({ id, name, bookId, i18n, languageFile }) {
  const entries = [["fabric.mod.json", fabricMetadata(id, name)]];
  if (languageFile) entries.push(["assets/fixture/lang/en_us.json", json(languageFile)]);
  entries.push(
    [`data/fixture/patchouli_books/${bookId}/book.json`, json({
      name: i18n ? "book.fixture.name" : "Field Guide",
      landing_text: i18n ? "book.fixture.landing" : "Find the $(item) relic in the ruins.",
      i18n,
    })],
    [`assets/fixture/patchouli_books/${bookId}/en_us/categories/basics.json`, json({
      name: i18n ? "category.fixture.basics" : "First Steps",
      description: i18n ? "category.fixture.basics.description" : "A short guide to $(item).",
      icon: "minecraft:book",
    })],
    [`assets/fixture/patchouli_books/${bookId}/en_us/entries/start.json`, json({
      name: i18n ? "entry.fixture.start" : "The Ancient Relic",
      category: "fixture:basics",
      icon: "minecraft:book",
      pages: [{
        type: "patchouli:text",
        text: i18n ? "entry.fixture.start.page" : "Use $(item) to identify the ancient relic.",
      }],
    })],
  );
  return makeZip(entries);
}

async function writeText(root, relativePath, contents) {
  const destination = path.join(root, relativePath);
  await mkdir(path.dirname(destination), { recursive: true });
  await writeFile(destination, contents, "utf8");
}

export async function generateFeatureFixtures(outputDirectory) {
  assert.ok(outputDirectory, "fixture の出力先を指定してください。");
  const single = path.join(outputDirectory, "single-instance");
  const split = path.join(outputDirectory, "split-instance");
  const legacy = path.join(outputDirectory, "legacy-instance");
  const mods = path.join(single, "mods");
  await Promise.all([mkdir(mods, { recursive: true }), mkdir(path.join(split, "mods"), { recursive: true }), mkdir(path.join(legacy, "mods"), { recursive: true })]);

  await writeFile(path.join(mods, "patchouli-i18n.jar"), patchouliJar({
    id: "fixture_i18n_guide",
    name: "Fixture i18n Guide",
    bookId: "i18n_guide",
    i18n: true,
    languageFile: {
      "book.fixture.name": "Fixture Guide",
      "book.fixture.landing": "A translated guide",
      "category.fixture.basics": "Basics",
      "category.fixture.basics.description": "A guide category",
      "entry.fixture.start": "Starting the guide",
      "entry.fixture.start.page": "Read the first page",
      "block.fixture.relic": "Ancient Relic %s",
    },
  }));
  await writeFile(path.join(mods, "patchouli-body-only.jar"), patchouliJar({
    id: "fixture_body_guide",
    name: "Fixture Body Guide",
    bookId: "body_guide",
    i18n: false,
    languageFile: null,
  }));

  await mkdir(path.join(single, "shaderpacks"), { recursive: true });
  await writeFile(path.join(single, "shaderpacks", "Aurora.zip"), makeZip([
    ["shaders/lang/en_US.lang", "option.BLOOM=Bloom\noption.BLOOM.comment=Controls bloom intensity\nscreen.LIGHTING=Lighting\nvalue.BLOOM.HIGH=High\noption.EXPOSURE=Exposure %s\n"],
  ]));
  await writeText(single, "options.txt", "version:1\n");
  await writeText(single, "config/ftbquests/quests/lang/en_us.snbt", `{
  quest.fixture.title: "&aA green quest"
  quest.fixture.description: [
    "&aFind the relic {@pagebreak} {image:fixture:map}"
  ]
}\n`);
  await writeText(single, "config/ftbquests/quests/lang/ja_jp.snbt", `{
  quest.fixture.title: "&a元のクエスト"
  quest.fixture.description: [
    "&a元の説明 {@pagebreak} {image:fixture:map}"
  ]
}\n`);
  await writeText(single, "kubejs/assets/fixture/lang/en_us.json", json({
    "item.fixture.widget": "Widget",
  }));

  await writeText(split, "config/ftbquests/quests/lang/en_us/chapters/start.snbt", `{
  quest.split.title: "&bSplit quest {@pagebreak}"
}\n`);
  await writeText(legacy, "config/ftbquests/quests/chapters/legacy.snbt", `{
  quests: [{ title: "Inline quest text" }]
}\n`);
  return { singleInstance: single, splitInstance: split, legacyInstance: legacy };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const outputDirectory = process.argv[2];
  assert.ok(outputDirectory, "Usage: node generate-feature-fixtures.mjs <output-directory>");
  const fixtures = await generateFeatureFixtures(path.resolve(outputDirectory));
  console.log(JSON.stringify(fixtures, null, 2));
}
