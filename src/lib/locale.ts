import { UI_LANGUAGE_INFO } from "../i18n";
import { getUiLanguage } from "../i18n/store";

export function localeDisplayName(locale: string): string {
  try {
    const language = getUiLanguage();
    return new Intl.DisplayNames([UI_LANGUAGE_INFO[language].intlTag], { type: "language" })
      .of(locale.replaceAll("_", "-")) ?? locale;
  } catch {
    // JAR名から得た不正なlocale文字列でも画面を止めず、元の識別子を表示する。
    return locale;
  }
}
