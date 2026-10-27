/** @vitest-environment jsdom */

import { cleanup, render, screen } from "@testing-library/react";
import { afterAll, afterEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const globalStyles = readFileSync(resolve(process.cwd(), "src/styles/global.css"), "utf8");

const stylesheet = document.createElement("style");
// jsdom 30 は疑似要素を含むセレクターリストを通常要素の計算スタイルへ適用できない。
stylesheet.textContent = globalStyles.replace(/^\*,\r?\n\*::before,\r?\n\*::after\s*\{/m, "* {");
document.head.append(stylesheet);

afterEach(cleanup);
afterAll(() => stylesheet.remove());

describe("text selection defaults", () => {
  it("入力欄と .selectable だけ選択でき、通常の画面文言は選択できない", () => {
    render(
      <>
        <h1 data-testid="heading">見出し</h1>
        <button data-testid="button">操作</button>
        <p className="selectable" data-testid="selectable">保存先のパス</p>
        <input data-testid="input" value="検索語" readOnly />
        <textarea data-testid="textarea" value="編集中の訳" readOnly />
      </>,
    );

    expect(window.getComputedStyle(screen.getByTestId("heading")).userSelect).toBe("none");
    expect(window.getComputedStyle(screen.getByTestId("button")).userSelect).toBe("none");
    expect(window.getComputedStyle(screen.getByTestId("selectable")).userSelect).toBe("text");
    expect(window.getComputedStyle(screen.getByTestId("input")).userSelect).toBe("text");
    expect(window.getComputedStyle(screen.getByTestId("textarea")).userSelect).toBe("text");
  });

  it("解析エラーの技術詳細はコピーできる", () => {
    render(<div className="analyze-screen__error"><pre>archive error detail</pre></div>);

    expect(window.getComputedStyle(screen.getByText("archive error detail")).userSelect).toBe("text");
  });
});
