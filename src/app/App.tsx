import { useEffect, useLayoutEffect } from "react";

import { AnalyzeScreen } from "../features/analyze/AnalyzeScreen";
import { LoadingScreen } from "../features/analyze/LoadingScreen";
import { ExportCompletionScreen } from "../features/export/ExportCompletionScreen";
import { ExportScreen } from "../features/export/ExportScreen";
import { HomeScreen } from "../features/home/HomeScreen";
import { ReviewScreen } from "../features/review/ReviewScreen";
import { ScanScreen } from "../features/scan/ScanScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { useAppStore } from "../stores/appStore";
import { useSettingsStore } from "../stores/settingsStore";
import { checkForUpdateOnStartup } from "../features/updater/updater";
import { IconButton, TertiaryButton } from "../components/button/Buttons";
import { useContextMenuPolicy } from "../components/ContextMenu";
import { Tooltip } from "../components/Controls";
import { TranslationScreen } from "../features/translation/TranslationScreen";
import "./App.css";

export function App() {
  useContextMenuPolicy();
  const route = useAppStore((state) => state.route);
  const settingsReturnRoute = useAppStore((state) => state.settingsReturnRoute);
  const goTo = useAppStore((state) => state.goTo);
  const openSettings = useAppStore((state) => state.openSettings);
  const loadSettings = useSettingsStore((state) => state.load);
  const settingsLoaded = useSettingsStore((state) => state.loaded);
  const appearance = useSettingsStore((state) => state.settings["appearance.theme"]);
  const homeDisabled = route === "home" || route === "loading";
  const goHome = () => {
    if (!homeDisabled) void goTo("home");
  };

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  useEffect(() => {
    if (settingsLoaded) checkForUpdateOnStartup();
  }, [settingsLoaded]);

  useEffect(() => {
    const handleShortcut = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.altKey || event.key.toLowerCase() !== "o") return;
      event.preventDefault();
      // 用語集や確認ダイアログの入力・判断を、背面の画面遷移で失わないようにする。
      if (document.querySelector('[aria-modal="true"]')) return;
      void (async () => {
        await goTo("home");
        if (useAppStore.getState().route !== "home") return;
        // File pickerはHomeScreenが所有するため、画面が切り替わった後に要求を渡す。
        requestAnimationFrame(() => requestAnimationFrame(() => window.dispatchEvent(new Event("yakucraft:open-jar"))));
      })();
    };
    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, [goTo]);

  useLayoutEffect(() => {
    if (!settingsLoaded) return;

    const root = document.documentElement;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const applyTheme = () => {
      const dark = appearance === "dark" || (appearance !== "light" && media.matches);
      root.dataset.theme = dark ? "dark" : "light";
    };
    applyTheme();
    media.addEventListener("change", applyTheme);
    return () => media.removeEventListener("change", applyTheme);
  }, [appearance, settingsLoaded]);

  // 初回起動ではcacheが未作成の場合があるため、DBの保存テーマ確定前は画面を描画しない。
  if (!settingsLoaded) return null;

  const displayRoute = route === "settings" ? settingsReturnRoute : route;
  const showExportCompletion = displayRoute === "exportComplete";
  const screenRoute = showExportCompletion ? "export" : displayRoute;

  let screen;
  switch (screenRoute) {
    case "home":
      screen = <HomeScreen />;
      break;
    case "analyze":
      screen = <AnalyzeScreen />;
      break;
    case "scan":
      screen = <ScanScreen />;
      break;
    case "loading":
      screen = <LoadingScreen />;
      break;
    case "review":
      screen = <ReviewScreen />;
      break;
    case "editor":
      screen = <TranslationScreen />;
      break;
    case "export":
      screen = <ExportScreen />;
      break;
  }

  return (
    <div className="app-shell">
      <header className="app-header">
        <button className="app-brand" type="button" aria-label="ホームへ戻る" disabled={homeDisabled} onClick={goHome}>
          {/* アプリアイコン（src-tauri/icons/app-icon.svg）と同じ形。黒い枝は currentColor にして、
              ダークテーマでも文字色に追従して見えるようにする。緑はブランド色のまま固定する。 */}
          <svg className="app-brand__mark" viewBox="196 196 632 668" aria-hidden="true" focusable="false">
            <defs>
              <clipPath id="app-brand-right-half">
                <rect x="512" y="0" width="512" height="1024" />
              </clipPath>
            </defs>
            <path d="M300 300 L512 512 L512 760" stroke="currentColor" strokeWidth="128" strokeLinecap="round" strokeLinejoin="round" fill="none" />
            <path d="M724 300 L512 512" stroke="var(--color-brand-accent)" strokeWidth="128" strokeLinecap="round" strokeLinejoin="round" fill="none" clipPath="url(#app-brand-right-half)" />
          </svg>
          <span>YakuCraft</span>
        </button>
        {route === "home" ? (
          <nav className="app-header__actions" aria-label="画面操作">
            <TertiaryButton onClick={() => void openSettings("general")}>設定</TertiaryButton>
          </nav>
        ) : (
          <nav className="app-header__actions" aria-label="画面操作">
            <Tooltip content="ホームへ戻る">
              <IconButton
                aria-label="ホームへ戻る"
                disabled={homeDisabled}
                onClick={goHome}
              >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="m3 10 9-7 9 7v10a1 1 0 0 1-1 1h-5v-7H9v7H4a1 1 0 0 1-1-1z" />
                </svg>
              </IconButton>
            </Tooltip>
            {route !== "settings" && route !== "loading" && route !== "analyze" && (
              <TertiaryButton onClick={() => void openSettings("general")}>設定</TertiaryButton>
            )}
          </nav>
        )}
      </header>
      <main className="app-main" id="main-content" tabIndex={-1}>
        {/* hiddenでも元の画面をマウントしたままにし、表のスクロールや各画面の入力状態を保つ。 */}
        <div className="app-main__screen" hidden={route === "settings" || showExportCompletion}>
          {screen}
        </div>
        {showExportCompletion && (
          <div className="app-main__screen" hidden={route === "settings"}>
            <ExportCompletionScreen />
          </div>
        )}
        {route === "settings" && <SettingsScreen />}
      </main>
    </div>
  );
}
