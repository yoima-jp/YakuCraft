import { open as openNativeDialog, save as saveNativeDialog } from "@tauri-apps/plugin-dialog";

interface E2EDialogPaths {
  jarPath?: string;
  directoryPath?: string;
  exportPath?: string;
}

declare global {
  interface Window {
    __YAKUCRAFT_E2E__?: E2EDialogPaths;
  }
}

export async function openFileDialog(
  options?: Parameters<typeof openNativeDialog>[0],
): Promise<string | string[] | null> {
  if (import.meta.env.VITE_E2E === "1") {
    // ネイティブダイアログはWebDriverから操作できないため、E2Eビルド内だけ注入パスを返す。
    return window.__YAKUCRAFT_E2E__?.[options?.directory ? "directoryPath" : "jarPath"] ?? null;
  }
  return openNativeDialog(options);
}

export async function saveFileDialog(
  options?: Parameters<typeof saveNativeDialog>[0],
): Promise<string | null> {
  if (import.meta.env.VITE_E2E === "1") {
    // 本番ビルドではViteが上の定数分岐を除去するため、テスト用window参照も成果物に残らない。
    return window.__YAKUCRAFT_E2E__?.exportPath ?? null;
  }
  return saveNativeDialog(options);
}
