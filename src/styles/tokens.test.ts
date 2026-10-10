import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

type Tokens = Record<string, string>;

const css = readFileSync("src/styles/tokens.css", "utf8");
const themes: Array<[string, string]> = [
  ["ライト", ":root"],
  ["ダーク", ':root[data-theme="dark"]'],
];

const contrastPairs: Array<[string, string, string, number?]> = [
  ["本文と背景", "--color-text-primary", "--color-background"],
  // PrimaryButton は背景色トークンを文字色に使うため、実際の組み合わせを測る。
  ["Primary Button", "--color-background", "--color-primary"],
  ["補助文字とSurface", "--color-text-secondary", "--color-surface"],
  ["Redと背景", "--color-red", "--color-background"],
  ["Blueと背景", "--color-blue", "--color-background"],
  ["入力文字と浮き上がったSurface", "--color-text-primary", "--color-surface-raised"],
  ["選択項目の文字", "--color-text-primary", "--color-primary-surface"],
  ["フォーカスリングと背景", "--focus-ring-color", "--color-background", 3],
  ["フォーカスリングと選択面", "--focus-ring-color", "--color-selection-surface", 3],
  ["選択文字", "--color-selection-text", "--color-selection-surface"],
  ["選択マーク", "--color-background", "--color-primary-dark"],
  ["Destructive Button", "--color-background", "--color-red"],
  ["Tooltip", "--color-background", "--color-text-primary"],
];

function readTokenBlock(selector: string): Tokens {
  const selectorStart = css.indexOf(selector);
  const openingBrace = css.indexOf("{", selectorStart);
  const closingBrace = css.indexOf("}", openingBrace);
  if (selectorStart < 0 || openingBrace < 0 || closingBrace < 0) {
    throw new Error(`${selector} の色トークンが見つかりません。`);
  }
  const block = css.slice(openingBrace + 1, closingBrace);

  return Object.fromEntries(
    [...block.matchAll(/^\s*(--[\w-]+):\s*(#[\da-fA-F]{6}|var\(--[\w-]+\))\s*;/gm)]
      .map(([, name, value]) => [name, value]),
  );
}

function readThemeTokens(selector: string): Tokens {
  const baseTokens = selector === ":root" ? {} : readTokenBlock(":root");
  return { ...baseTokens, ...readTokenBlock(selector) };
}

function resolveToken(tokens: Tokens, name: string, visited = new Set<string>()): string {
  if (visited.has(name)) throw new Error(`${name} のToken参照が循環しています。`);
  const value = tokens[name];
  if (!value) throw new Error(`${name} の色Tokenがありません。`);
  const reference = value.match(/^var\((--[\w-]+)\)$/);
  if (!reference) return value;
  visited.add(name);
  return resolveToken(tokens, reference[1], visited);
}

function relativeLuminance(hex: string): number {
  const channels = hex.match(/[\da-fA-F]{2}/g);
  if (!channels || channels.length !== 3) throw new Error(`色の形式が正しくありません: ${hex}`);
  const [red, green, blue] = channels.map((channel) => {
    const value = parseInt(channel, 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

function contrastRatio(foreground: string, background: string): number {
  const first = relativeLuminance(foreground);
  const second = relativeLuminance(background);
  return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
}

describe("テーマ色のコントラスト", () => {
  it.each(themes)("%sテーマの主要な色の組み合わせが4.5:1以上", (theme, selector) => {
    const tokens = readThemeTokens(selector);
    for (const [label, foregroundToken, backgroundToken, minimum = 4.5] of contrastPairs) {
      const foreground = resolveToken(tokens, foregroundToken);
      const background = resolveToken(tokens, backgroundToken);
      expect(foreground, `${theme}: ${foregroundToken} がありません`).toBeDefined();
      expect(background, `${theme}: ${backgroundToken} がありません`).toBeDefined();
      expect(
        contrastRatio(foreground, background),
        `${theme}: ${label} (${foreground} / ${background})`,
      ).toBeGreaterThanOrEqual(minimum);
    }
  });

  it("ダークテーマのPreview背景は明暗どちらのTextureとも判別できる", () => {
    const tokens = readThemeTokens(':root[data-theme="dark"]');
    const background = tokens["--color-preview-background"];
    expect(background, "ダークテーマのプレビュー背景色がありません").toBeDefined();
    expect(contrastRatio("#000000", background)).toBeGreaterThanOrEqual(3);
    expect(contrastRatio("#ffffff", background)).toBeGreaterThanOrEqual(3);
  });
});
