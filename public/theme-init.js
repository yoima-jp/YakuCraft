(() => {
  let theme = "system";
  try {
    const cached = window.localStorage.getItem("yakucraft.appearance.theme");
    if (cached === "light" || cached === "dark" || cached === "system") theme = cached;
  } catch {
    // WebView storageが使えない場合はOSテーマで初期表示する。
  }

  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  // Tauriのscript-src 'self' CSPを保つため、初期テーマはheadから読む外部scriptで設定する。
  document.documentElement.dataset.theme = theme === "dark" || (theme === "system" && prefersDark)
    ? "dark"
    : "light";
})();
