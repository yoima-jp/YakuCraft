import { isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { create } from "zustand";

import { flushBeforeLeavingEditor } from "../../lib/navigation";
import { logFrontendError } from "../../lib/errorLogging";
import { automaticUpdatesSupported } from "../../lib/tauri";
import { useSettingsStore } from "../../stores/settingsStore";

const LAST_STARTUP_CHECK_KEY = "yakucraft.updater.last-startup-check";

interface UpdaterState {
  updatesSupported: boolean | null;
  availableUpdate: Update | null;
  checking: boolean;
  installing: boolean;
  downloadedBytes: number;
  contentLength: number | null;
}

export const useUpdaterStore = create<UpdaterState>()(() => ({
  updatesSupported: null,
  availableUpdate: null,
  checking: false,
  installing: false,
  downloadedBytes: 0,
  contentLength: null,
}));

let checkRequest: Promise<Update | null> | null = null;
let supportRequest: Promise<boolean> | null = null;
let startupCheckStarted = false;

export function loadUpdateSupport() {
  const cached = useUpdaterStore.getState().updatesSupported;
  if (cached !== null) return Promise.resolve(cached);
  if (supportRequest) return supportRequest;

  supportRequest = (import.meta.env.DEV
    ? Promise.resolve(true)
    : isTauri()
      ? automaticUpdatesSupported()
      : Promise.resolve(false))
    .then((supported) => {
      useUpdaterStore.setState({ updatesSupported: supported });
      return supported;
    })
    .catch((cause: unknown) => {
      console.warn("更新の対応状況を確認できませんでした。", cause);
      void logFrontendError(cause, "update support check");
      useUpdaterStore.setState({ updatesSupported: false });
      return false;
    })
    .finally(() => {
      supportRequest = null;
    });
  return supportRequest;
}

async function canCheckForUpdates() {
  return isTauri() && !import.meta.env.DEV && await loadUpdateSupport();
}

function localDay() {
  const date = new Date();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

export async function checkForUpdate() {
  if (!(await canCheckForUpdates())) return null;
  const state = useUpdaterStore.getState();
  if (state.installing) return state.availableUpdate;
  if (checkRequest) return checkRequest;

  useUpdaterStore.setState({ checking: true });
  const request = Promise.resolve()
    .then(() => check())
    .then(async (update) => {
      const previous = useUpdaterStore.getState().availableUpdate;
      if (useUpdaterStore.getState().installing) {
        if (update && update !== previous) {
          try {
            await update.close();
          } catch (cause: unknown) {
            console.warn("確認中に取得した更新情報を解放できませんでした。", cause);
          }
        }
        return previous;
      }
      if (previous && previous !== update) {
        try {
          await previous.close();
        } catch (cause: unknown) {
          console.warn("以前の更新情報を解放できませんでした。", cause);
        }
      }
      useUpdaterStore.setState({ availableUpdate: update });
      return update;
    })
    .finally(() => {
      useUpdaterStore.setState({ checking: false });
      checkRequest = null;
    });
  checkRequest = request;
  return request;
}

export function checkForUpdateOnStartup() {
  if (startupCheckStarted) return;
  startupCheckStarted = true;
  void (async () => {
    if (
      !(await canCheckForUpdates())
      || !useSettingsStore.getState().value<boolean>("updates.checkOnStartup")
    ) return;

    const today = localDay();
    try {
      if (window.localStorage.getItem(LAST_STARTUP_CHECK_KEY) === today) return;
      window.localStorage.setItem(LAST_STARTUP_CHECK_KEY, today);
    } catch (cause: unknown) {
      console.warn("更新確認日の保存に失敗しました。", cause);
      void logFrontendError(cause, "save update check date");
    }

    await checkForUpdate();
  })().catch((cause: unknown) => {
    console.warn("起動時の更新確認に失敗しました。", cause);
    void logFrontendError(cause, "startup update check");
  });
}

export async function installAvailableUpdate() {
  if (!(await canCheckForUpdates())) return;
  if (checkRequest) {
    try {
      await checkRequest;
    } catch (cause: unknown) {
      console.error("更新を確認できなかったため、適用を中止しました。", cause);
      void logFrontendError(cause, "update check before install");
      return;
    }
  }

  const { availableUpdate, installing } = useUpdaterStore.getState();
  if (!availableUpdate || installing) return;

  useUpdaterStore.setState({
    installing: true,
    downloadedBytes: 0,
    contentLength: null,
  });

  try {
    if (!(await flushBeforeLeavingEditor())) {
      console.warn("保存できないため、更新を中止しました。");
      useUpdaterStore.setState({ installing: false });
      return;
    }

    await availableUpdate.download((event) => {
      switch (event.event) {
        case "Started":
          useUpdaterStore.setState({ downloadedBytes: 0, contentLength: event.data.contentLength ?? null });
          break;
        case "Progress":
          useUpdaterStore.setState((state) => ({
            downloadedBytes: state.downloadedBytes + event.data.chunkLength,
          }));
          break;
        case "Finished":
          break;
      }
    });

    // 更新のダウンロード中に翻訳を編集できるため、Windows が終了する直前にも保存状態を確かめる。
    if (!(await flushBeforeLeavingEditor())) {
      console.warn("保存できないため、更新を中止しました。");
      useUpdaterStore.setState({ installing: false });
      return;
    }

    await availableUpdate.install();
    await relaunch();
  } catch (cause: unknown) {
    console.error("更新を適用できませんでした。", cause);
    void logFrontendError(cause, "install update");
    useUpdaterStore.setState({ installing: false });
  }
}
