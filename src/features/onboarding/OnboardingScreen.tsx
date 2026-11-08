import { useEffect, useState } from "react";

import { RadioGroup } from "../../components/Controls";
import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { TargetLanguagePicker } from "../../components/TargetLanguagePicker";
import { autoTargetLocale, detectTargetLocale } from "../../i18n/detect";
import { UI_LANGUAGES, UI_LANGUAGE_INFO, useT, useUiLanguage, setUiLanguage } from "../../i18n";
import type { UiLanguage } from "../../i18n/uiLanguages";
import { getPlamoStatus } from "../../lib/tauri";
import { useSettingsStore } from "../../stores/settingsStore";
import "./OnboardingScreen.css";

export function OnboardingScreen() {
  const language = useUiLanguage();
  const translate = useT();
  const systemLocale = useSettingsStore((state) => state.systemLocale);
  const save = useSettingsStore((state) => state.save);
  const savedProvider = useSettingsStore((state) => state.settings["ai.provider"]);
  const osLocale = systemLocale ?? (typeof navigator === "undefined" ? null : navigator.languages[0] ?? null);
  const [targetLocale, setTargetLocale] = useState(() => detectTargetLocale(osLocale));
  const [targetStep, setTargetStep] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [plamoSupportedLocales, setPlamoSupportedLocales] = useState<string[] | null>(null);

  // 対応言語は PLaMo のときだけ必要なので、他プロバイダーでは取得しない。
  useEffect(() => {
    if (savedProvider !== "plamo") return;
    let active = true;
    void getPlamoStatus()
      .then((status) => {
        if (active) setPlamoSupportedLocales(status.supportedTargetLocales.map((code) => code.toLowerCase()));
      })
      .catch(() => {
        // 取得に失敗した場合は null のままにし、翻訳先の変更を止めて非対応言語の保存を防ぐ。
      });
    return () => { active = false; };
  }, [savedProvider]);

  // 対応言語が未取得のまま選ばせると、非対応の翻訳先を保存できてしまう。
  const targetPickerLocked = savedProvider === "plamo" && plamoSupportedLocales === null;

  const complete = async () => {
    if (saving) return;
    setSaving(true);
    setError(null);
    const selectedTargetLocale = language === "en"
      ? targetLocale
      : autoTargetLocale(language, osLocale);
    try {
      await save({
        "language.uiLanguage": language,
        "language.targetLocale": selectedTargetLocale,
        "onboarding.completed": true,
      });
    } catch {
      setError(translate("onboarding.saveError"));
    } finally {
      setSaving(false);
    }
  };

  const languageOptions = UI_LANGUAGES.map((value) => ({
    value,
    label: UI_LANGUAGE_INFO[value].nativeName,
    id: `onboarding-language-${value}`,
  }));

  return (
    <main className="onboarding-screen" aria-labelledby="onboarding-title">
      <section className="onboarding-card">
        <h1 className="page-title" id="onboarding-title">{translate("onboarding.title")}</h1>
        {!targetStep ? (
          <>
            <RadioGroup
              className="onboarding-language-options"
              legend={translate("onboarding.uiLanguage")}
              legendClassName="onboarding-card__legend"
              name="onboarding-ui-language"
              value={language}
              onChange={(value) => {
                if (UI_LANGUAGES.includes(value as UiLanguage)) setUiLanguage(value as UiLanguage);
              }}
              options={languageOptions}
            />
            <div className="onboarding-card__actions">
              {language === "en" ? (
                <PrimaryButton type="button" onClick={() => setTargetStep(true)}>{translate("onboarding.next")}</PrimaryButton>
              ) : (
                <PrimaryButton type="button" disabled={saving} onClick={() => void complete()}>
                  {translate("onboarding.complete")}
                </PrimaryButton>
              )}
            </div>
          </>
        ) : (
          <>
            <label className="settings-field" htmlFor="onboarding-target-language">
              {translate("onboarding.targetLanguage")}
              <TargetLanguagePicker
                id="onboarding-target-language"
                aria-label={translate("onboarding.targetLanguage")}
                value={targetLocale}
                disabled={targetPickerLocked}
                onChange={setTargetLocale}
                isOptionDisabled={plamoSupportedLocales
                  ? (code) => !plamoSupportedLocales.includes(code.toLowerCase())
                  : undefined}
                disabledOptionReason={translate("settings.plamoUnsupportedLanguage")}
              />
            </label>
            <div className="onboarding-card__actions">
              <SecondaryButton type="button" disabled={saving} onClick={() => setTargetStep(false)}>
                {translate("onboarding.back")}
              </SecondaryButton>
              <PrimaryButton type="button" disabled={saving} onClick={() => void complete()}>
                {translate("onboarding.complete")}
              </PrimaryButton>
            </div>
          </>
        )}
        {error && <p className="settings-message settings-message--error" role="alert">{error}</p>}
      </section>
    </main>
  );
}
