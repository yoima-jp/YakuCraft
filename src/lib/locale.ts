const languageNames = new Intl.DisplayNames(["ja"], { type: "language" });

export function localeDisplayName(locale: string): string {
  try {
    return languageNames.of(locale.replaceAll("_", "-")) ?? locale;
  } catch {
    // JAR名から得た不正なlocale文字列でも画面を止めず、元の識別子を表示する。
    return locale;
  }
}
