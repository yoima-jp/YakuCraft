import { describe, expect, it, vi } from "vitest";

import type { TranslationEntry } from "../../types/translation";
import { createThumbnailQueue } from "../preview/thumbnails";

function job(requestKey: string) {
  const entry: TranslationEntry = {
    id: requestKey.charCodeAt(0),
    namespace: "sample",
    key: `block.sample.${requestKey}`,
    sourceText: null,
    sourceLocale: null,
    targetText: "",
    origin: "missing",
    flags: [],
  };
  return { requestKey, entry, projectId: "project" };
}

describe("preview thumbnail queue", () => {
  it("アイドル処理を待ってから背景行を進め、可視行を先に処理する", async () => {
    const rendered: string[] = [];
    let releaseFirst: (() => void) | undefined;
    let markStarted: (() => void) | undefined;
    const firstStarted = new Promise<void>((resolve) => { markStarted = resolve; });
    let active = 0;
    let maxActive = 0;
    const queue = createThumbnailQueue({
      render: async (item) => {
        rendered.push(item.requestKey);
        active += 1;
        maxActive = Math.max(maxActive, active);
        if (item.requestKey === "background-a") {
          markStarted?.();
          await new Promise<void>((resolve) => { releaseFirst = resolve; });
        }
        active -= 1;
        return { cacheKey: item.requestKey, blob: new Blob([item.requestKey]) };
      },
      createObjectUrl: (blob) => `blob:${blob.size}:${rendered.at(-1)}`,
      revokeObjectUrl: vi.fn(),
    });

    queue.setBackground([job("background-a"), job("background-b")]);
    expect(rendered).toEqual([]);
    await firstStarted;
    queue.setVisible([job("visible-a"), job("visible-b")]);
    releaseFirst?.();
    await queue.waitForIdle();

    expect(rendered).toEqual(["background-a", "visible-a", "visible-b", "background-b"]);
    expect(maxActive).toBe(1);
    queue.dispose();
  });

  it("可視行だけを直列処理し、スクロール中に古い待ち行を外す", async () => {
    let releaseFirst: (() => void) | undefined;
    let markStarted: (() => void) | undefined;
    const firstStarted = new Promise<void>((resolve) => { markStarted = resolve; });
    const rendered: string[] = [];
    let active = 0;
    let maxActive = 0;
    const queue = createThumbnailQueue({
      render: async (item) => {
        rendered.push(item.requestKey);
        active += 1;
        maxActive = Math.max(maxActive, active);
        if (item.requestKey === "a") {
          markStarted?.();
          await new Promise<void>((resolve) => { releaseFirst = resolve; });
        }
        active -= 1;
        return { cacheKey: item.requestKey, blob: new Blob([item.requestKey]) };
      },
      createObjectUrl: (blob) => `blob:${blob.size}:${rendered.at(-1)}`,
      revokeObjectUrl: vi.fn(),
    });

    queue.setVisible([job("a"), job("b")]);
    await firstStarted;
    queue.setVisible([job("c")]);
    releaseFirst?.();
    await queue.waitForIdle();

    expect(rendered).toEqual(["a", "c"]);
    expect(maxActive).toBe(1);
    expect(queue.get("b")).toBeNull();
    expect(queue.get("c")).toBe("blob:1:c");
    queue.dispose();
  });

  it("上限を超えた画像を古い順に破棄してURLをrevokeする", async () => {
    const revoked: string[] = [];
    let nextUrl = 0;
    const queue = createThumbnailQueue({
      maxCacheSize: 2,
      render: async (item) => ({ cacheKey: item.requestKey, blob: new Blob([item.requestKey]) }),
      createObjectUrl: () => `blob:${++nextUrl}`,
      revokeObjectUrl: (url) => revoked.push(url),
    });

    queue.setVisible([job("a"), job("b"), job("c")]);
    await queue.waitForIdle();

    expect(queue.cacheSize()).toBe(2);
    expect(revoked).toEqual(["blob:1"]);
    expect(queue.get("a")).toBeNull();
    expect(queue.get("b")).toBe("blob:2");
    queue.dispose();
    expect(revoked).toEqual(["blob:1", "blob:3", "blob:2"]);
  });
});
