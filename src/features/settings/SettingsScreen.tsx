import { useCallback, useEffect, useRef, useState } from "react";

import { AppCommandError, cancelChatgptLogin, clearCache, deleteAiProvider, getAiSettings, getAppInfo, getPlamoStatus, getRecentLog, listAiModels, logoutChatgpt, openLogFolder, saveAiProvider, startChatgptLogin, testAiProvider } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { openFileDialog } from "../../lib/dialogs";
import { isImeCompositionKeyDown } from "../../lib/keyboard";
import { Checkbox, Combobox, Disclosure, NumberInput, RadioGroup, Select } from "../../components/Controls";
import { SecondaryButton } from "../../components/button/Buttons";
import { useAppStore } from "../../stores/appStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import { checkForUpdate, loadUpdateSupport, useUpdaterStore } from "../updater/updater";
import type { AiProviderId, AiSettingsDto, PlamoStatusDto } from "../../types/ai";
import type { AppInfoDto } from "../../types/settings";
import { MinecraftJarSection } from "./MinecraftJarSection";
import { PlamoSection } from "./PlamoSection";
import { TargetLanguagePicker } from "../../components/TargetLanguagePicker";
import { UI_LANGUAGES, UI_LANGUAGE_INFO, isUiLanguage, t, useT, useUiLanguage, setUiLanguage } from "../../i18n";
import "./SettingsScreen.css";

type SettingsCategory = "general" | "language" | "ai" | "editor" | "appearance" | "advanced" | "logs";
type SettingsCategoryKey =
  | "settings.generalCategory"
  | "settings.languageCategory"
  | "settings.aiCategory"
  | "settings.editorCategory"
  | "settings.appearanceCategory"
  | "settings.advancedCategory"
  | "settings.logsCategory";
type SettingsProviderKey =
  | "settings.providerChatgpt"
  | "settings.providerOpenai"
  | "settings.providerClaude"
  | "settings.providerGemini"
  | "settings.providerOpenrouter"
  | "settings.providerDeepseek"
  | "settings.providerGroq"
  | "settings.providerOllama"
  | "settings.providerLmStudio"
  | "settings.providerOpenaiCompatible"
  | "settings.providerDeepl"
  | "settings.providerPlamo";

const CATEGORIES: Array<[SettingsCategory, SettingsCategoryKey]> = [
  ["general", "settings.generalCategory"],
  ["language", "settings.languageCategory"],
  ["ai", "settings.aiCategory"],
  ["editor", "settings.editorCategory"],
  ["appearance", "settings.appearanceCategory"],
  ["advanced", "settings.advancedCategory"],
  ["logs", "settings.logsCategory"],
];

// 既定の ChatGPT を先頭に置く（既定値は settingsStore と commands/ai.rs の provider_from_settings）。
const PROVIDERS: Array<[AiProviderId, SettingsProviderKey]> = [
  ["chatgpt", "settings.providerChatgpt"],
  ["openai", "settings.providerOpenai"],
  ["anthropic", "settings.providerClaude"],
  ["gemini", "settings.providerGemini"],
  ["openrouter", "settings.providerOpenrouter"],
  ["deepseek", "settings.providerDeepseek"],
  ["groq", "settings.providerGroq"],
  ["ollama", "settings.providerOllama"],
  ["lm-studio", "settings.providerLmStudio"],
  ["openai-compatible", "settings.providerOpenaiCompatible"],
  ["deepl", "settings.providerDeepl"],
  ["plamo", "settings.providerPlamo"],
];

function aiSaveConfig(settings: AiSettingsDto) {
  // PLaMo はローカル実行で接続先も追加指示も持たないため、契約どおり空文字を送る。
  const isPlamo = settings.provider === "plamo";
  return {
    provider: settings.provider,
    model: settings.model,
    baseUrl: isPlamo ? "" : settings.baseUrl,
    batchSize: settings.batchSize,
    concurrency: settings.concurrency,
    customPrompt: isPlamo ? "" : settings.customPrompt,
    // PLaMo だけが device を使う。他プロバイダーには送らず、既存の保存内容を変えない。
    ...(isPlamo ? { device: settings.device ?? "gpu" } : {}),
  };
}

function errorMessage(error: unknown) {
  void logFrontendError(error, "settings screen");
  return error instanceof AppCommandError && error.appError.code !== "unexpected"
    ? error.message
    : t("settings.genericError");
}

function stringSetting(settings: Record<string, unknown>, key: string, fallback: string) {
  const value = settings[key];
  return typeof value === "string" ? value : fallback;
}

function numberSetting(settings: Record<string, unknown>, key: string, fallback: number) {
  const value = settings[key];
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function LogViewer() {
  const translate = useT();
  const [content, setContent] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async (refreshing: boolean) => {
    if (refreshing) {
      setLoading(true);
      setError(null);
      setMessage(null);
    }
    try {
      setContent(await getRecentLog());
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  // ログ閲覧カテゴリを開いた時点でRustから内容を取得する。状態更新は非同期取得後に行う。
  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect
    void load(false);
  }, [load]);

  const reload = () => void load(true);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(content);
      setMessage(translate("settings.copied"));
      setError(null);
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  };

  const openFolder = async () => {
    try {
      await openLogFolder();
      setError(null);
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  };

  return (
    <div className="settings-fields">
      <div className="settings-actions">
        <button className="settings-action" type="button" disabled={loading} onClick={reload}>
          {translate("settings.reload")}
        </button>
        <button className="settings-action" type="button" disabled={!content} onClick={() => void copy()}>
          {translate("settings.copy")}
        </button>
        <button className="settings-action" type="button" onClick={() => void openFolder()}>
          {translate("settings.openLogFolder")}
        </button>
      </div>
      {loading && <output className="settings-field__hint" aria-live="polite">{translate("settings.loading")}</output>}
      {!loading && !content && <p className="settings-field__hint">{translate("settings.noLogs")}</p>}
      {content && <pre className="settings-log selectable" aria-label={translate("settings.logAria")}>{content}</pre>}
      {error && <p className="settings-message settings-message--error selectable" role="alert">{error}</p>}
      {message && <p className="settings-message" aria-live="polite">{message}</p>}
    </div>
  );
}

export function SettingsScreen() {
  const translate = useT();
  const uiLanguage = useUiLanguage();
  const category = useAppStore((state) => state.settingsCategory);
  const closeSettings = useAppStore((state) => state.closeSettings);
  const settings = useSettingsStore((state) => state.settings);
  const settingsLoaded = useSettingsStore((state) => state.loaded);
  const loadSettings = useSettingsStore((state) => state.load);
  const saveSettings = useSettingsStore((state) => state.save);
  const updatesSupported = useUpdaterStore((state) => state.updatesSupported);
  const updateInstalling = useUpdaterStore((state) => state.installing);
  const [ai, setAi] = useState<AiSettingsDto | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [modelOptions, setModelOptions] = useState<string[]>([]);
  const [modelError, setModelError] = useState<string | null>(null);
  const modelRequestGeneration = useRef(0);
  const [appInfo, setAppInfo] = useState<AppInfoDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [loginPending, setLoginPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [updateChecking, setUpdateChecking] = useState(false);
  // PLaMo の状態はパネルの表示と翻訳先の選択肢の両方で使うため、この画面で1回だけ取得して共有する。
  const [plamoStatus, setPlamoStatus] = useState<PlamoStatusDto | null>(null);
  const [plamoStatusFailed, setPlamoStatusFailed] = useState(false);
  const [plamoRefreshKey, setPlamoRefreshKey] = useState(0);
  // ダウンロード中にプロバイダーを切り替えると、完了後の再取得で表示が戻るため、切り替えを止める。
  const [plamoDownloading, setPlamoDownloading] = useState(false);
  const modelProvider = ai?.provider;
  const modelBaseUrl = ai?.baseUrl;
  const modelProviderConfigured = ai?.configured ?? false;
  const savedProvider = useSettingsStore((state) => state.settings["ai.provider"]);
  // 設定を読み込む前でも翻訳先の選択肢を絞れるよう、AI 欄の編集中の値を優先する。
  const effectiveProvider = (ai?.provider ?? savedProvider) as AiProviderId | undefined;
  // 別プロバイダーへ切り替えたときに、PLaMo の対応言語で選択肢を絞り続けないようにする。
  const plamoSupportedLocales = plamoStatus && effectiveProvider === "plamo"
    ? plamoStatus.supportedTargetLocales.map((code) => code.toLowerCase())
    : null;
  // 対応言語が未取得（取得中・失敗）のまま選ばせると、非対応の翻訳先を保存できてしまう。
  // 取得できるまでは変更を止め、選択済みの値は表示したままにする。
  const targetPickerLocked = effectiveProvider === "plamo" && plamoSupportedLocales === null;
  const targetLocale = stringSetting(settings, "language.targetLocale", String(SETTING_DEFAULTS["language.targetLocale"]));

  const refreshModels = useCallback(async (config: Pick<AiSettingsDto, "provider" | "baseUrl">, key?: string) => {
    const generation = ++modelRequestGeneration.current;
    setModelOptions([]);
    setModelError(null);
    if (config.provider === "deepl") {
      return;
    }
    try {
      const models = await listAiModels(config.provider, config.baseUrl, key || undefined);
      if (generation === modelRequestGeneration.current) setModelOptions(models);
    } catch {
      if (generation === modelRequestGeneration.current) {
        setModelOptions([]);
        setModelError(t("settings.modelListError"));
      }
    }
  }, []);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || isImeCompositionKeyDown(event)) return;
      event.preventDefault();
      void closeSettings();
    };
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [closeSettings]);

  useEffect(() => {
    if (!modelProvider || (modelProvider !== "chatgpt" && !modelBaseUrl?.trim())) return;
    const canListWithoutApiKey = modelProvider === "chatgpt"
      || modelProvider === "ollama"
      || modelProvider === "lm-studio"
      || modelProvider === "openai-compatible";
    if (!apiKey && !modelProviderConfigured && !canListWithoutApiKey) return;
    const timeout = window.setTimeout(() => {
      void refreshModels({ provider: modelProvider, baseUrl: modelBaseUrl ?? "" }, apiKey);
    }, 350);
    return () => window.clearTimeout(timeout);
  }, [apiKey, modelBaseUrl, modelProvider, modelProviderConfigured, refreshModels]);

  useEffect(() => {
    if (!settingsLoaded) void loadSettings();
  }, [loadSettings, settingsLoaded]);

  useEffect(() => {
    void loadUpdateSupport();
  }, []);

  useEffect(() => {
    void getAppInfo().then(setAppInfo).catch((cause: unknown) => setError(errorMessage(cause)));
    void getAiSettings()
      .then((settings) => {
        setAi(settings);
      })
      .catch((cause: unknown) => setError(errorMessage(cause)));
  }, [refreshModels]);

  // PLaMo の状態は、そのプロバイダーを表示しているときだけ取得する。
  useEffect(() => {
    if (effectiveProvider !== "plamo") return;
    let active = true;
    void getPlamoStatus()
      .then((status) => {
        if (!active) return;
        setPlamoStatus(status);
        setPlamoStatusFailed(false);
      })
      .catch((cause: unknown) => {
        if (!active) return;
        void logFrontendError(cause, "plamo status");
        setPlamoStatusFailed(true);
      });
    return () => { active = false; };
  }, [effectiveProvider, plamoRefreshKey]);

  const refreshPlamo = useCallback(async () => {
    try {
      const saved = await getAiSettings("plamo");
      // 取得中に別プロバイダーへ切り替えた場合、その選択を PLaMo で上書きしない。
      setAi((current) => (current && current.provider !== "plamo" ? current : saved));
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
    setPlamoRefreshKey((current) => current + 1);
  }, []);

  // ダウンロードは選択中のモデル・デバイスで行うため、先に保存してから開始する。
  const savePlamoSelection = useCallback(async () => {
    if (!ai || ai.provider !== "plamo") return false;
    setBusy(true);
    setError(null);
    try {
      setAi(await saveAiProvider(aiSaveConfig(ai)));
      await loadSettings();
      return true;
    } catch (cause: unknown) {
      setError(errorMessage(cause));
      return false;
    } finally {
      setBusy(false);
    }
  }, [ai, loadSettings]);

  const persist = useCallback(async (patch: Record<string, unknown>) => {
    setError(null);
    setMessage(null);
    try {
      await saveSettings(patch);
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  }, [saveSettings]);

  const selectProvider = async (provider: AiProviderId) => {
    modelRequestGeneration.current += 1;
    setModelOptions([]);
    setModelError(null);
    setBusy(true);
    setError(null);
    try {
      const settings = await getAiSettings(provider);
      setAi(settings);
      setApiKey("");
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const saveProvider = async () => {
    if (!ai) return;
    if (ai.provider !== "deepl" && !ai.model.trim()) {
      setModelError(translate("settings.modelRequired"));
      return;
    }
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const saved = await saveAiProvider(aiSaveConfig(ai), apiKey || undefined);
      setAi(saved);
      setApiKey("");
      setMessage(translate("settings.saved"));
      await loadSettings();
      // 保存で configured とダウンロード状態が変わるため、PLaMo は設定と状態を取り直す。
      if (ai.provider === "plamo") await refreshPlamo();
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const beginChatgptLogin = async () => {
    if (!ai) return;
    setLoginPending(true);
    setError(null);
    setMessage(null);
    try {
      await startChatgptLogin();
      const authenticated = await getAiSettings("chatgpt");
      const saved = await saveAiProvider(aiSaveConfig({ ...authenticated, ...ai, provider: "chatgpt" }));
      setAi(saved);
      await loadSettings();
      setMessage(translate("settings.loggedIn"));
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      setLoginPending(false);
    }
  };

  const cancelLogin = async () => {
    try {
      await cancelChatgptLogin();
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  };

  const logout = async () => {
    if (!ai) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await logoutChatgpt();
      setAi(await getAiSettings("chatgpt"));
      setMessage(translate("settings.loggedOut"));
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const removeProvider = async () => {
    if (!ai) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await deleteAiProvider(ai.provider);
      await loadSettings();
      const provider = useSettingsStore.getState().value<AiProviderId>("ai.provider");
      setAi(await getAiSettings(provider));
      setApiKey("");
      setMessage(translate("settings.deleted"));
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const testProvider = async () => {
    if (!ai) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await testAiProvider(ai.provider);
      setMessage(translate("settings.connected"));
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const browseExportDirectory = async () => {
    try {
      const directory = await openFileDialog({ directory: true, multiple: false });
      if (typeof directory === "string") await persist({ "general.defaultExportDirectory": directory });
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  };

  const clearAppCache = async () => {
    setError(null);
    setMessage(null);
    try {
      await clearCache();
      setMessage(translate("settings.cacheCleared"));
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  };

  const checkUpdates = async () => {
    setUpdateChecking(true);
    setError(null);
    setMessage(null);
    try {
      const update = await checkForUpdate();
      setMessage(import.meta.env.DEV
        ? translate("settings.developmentUpdateUnavailable")
        : update ? translate("settings.updateAvailable", { version: update.version }) : translate("settings.upToDate"));
    } catch (cause: unknown) {
      console.warn("手動の更新確認に失敗しました。", cause);
      void logFrontendError(cause, "manual update check");
      setError(translate("settings.updateCheckFailed"));
    } finally {
      setUpdateChecking(false);
    }
  };

  const categoryLabel = translate(CATEGORIES.find(([key]) => key === category)?.[1] ?? "settings.generalCategory");

  return (
    <section className="settings-screen" aria-labelledby="settings-title">
      <h1 className="page-title" id="settings-title">{translate("settings.title")}</h1>
      <div className="settings-layout">
        <nav className="settings-nav" aria-label={translate("settings.categoriesAria")}>
          {CATEGORIES.map(([key, labelKey]) => (
            <button
              className={`settings-nav__item${category === key ? " settings-nav__item--selected" : ""}`}
              type="button"
              aria-current={category === key ? "page" : undefined}
              key={key}
              onClick={() => useAppStore.setState({ settingsCategory: key })}
            >
              {translate(labelKey)}
            </button>
          ))}
        </nav>
        <section className="settings-content" aria-labelledby="settings-category-title">
          <h2 id="settings-category-title">{categoryLabel}</h2>
          {category === "language" && (
            <div className="settings-fields">
              <div className="settings-field">
                <span>{translate("settings.uiLanguage")}</span>
                <Select
                  aria-label={translate("settings.uiLanguage")}
                  value={uiLanguage}
                  onChange={(value) => {
                    if (!isUiLanguage(value)) return;
                    setUiLanguage(value);
                    void persist({ "language.uiLanguage": value });
                  }}
                  options={UI_LANGUAGES.map((value) => ({ value, label: UI_LANGUAGE_INFO[value].nativeName }))}
                />
              </div>
              <label className="settings-field" htmlFor="settings-target-language">
                {translate("settings.targetLanguage")}
                <TargetLanguagePicker
                  id="settings-target-language"
                  aria-label={translate("settings.targetLanguage")}
                  value={targetLocale}
                  disabled={targetPickerLocked}
                  onChange={(value) => void persist({ "language.targetLocale": value })}
                  // 非対応言語を隠すと一覧から消えて理由も分からないため、選べない状態で残す。
                  isOptionDisabled={plamoSupportedLocales
                    ? (code) => !plamoSupportedLocales.includes(code.toLowerCase())
                    : undefined}
                  disabledOptionReason={translate("settings.plamoUnsupportedLanguage")}
                />
                <span className="settings-field__hint">{translate("settings.targetLanguageHelp")}</span>
                {targetLocale.toLowerCase() === "en_us" && (
                  <span className="settings-field__hint">{translate("settings.englishTargetWarning")}</span>
                )}
              </label>
            </div>
          )}
          {category === "general" && (
            <div className="settings-fields">
              <label className="settings-field" htmlFor="recent-project-count">
                {translate("settings.recentProjectsCount")}
                <NumberInput
                  id="recent-project-count"
                  min={1}
                  max={50}
                  key={numberSetting(settings, "general.recentProjectsCount", 10)}
                  defaultValue={numberSetting(settings, "general.recentProjectsCount", 10)}
                  onBlur={(event) => {
                    const value = Number(event.currentTarget.value);
                    if (Number.isInteger(value) && value >= 1 && value <= 50) {
                      void persist({ "general.recentProjectsCount": value });
                    }
                  }}
                />
              </label>
              <div className="settings-field">
                <span>{translate("settings.defaultExportLocation")}</span>
                <div className="settings-path-field">
                  <input
                    value={stringSetting(settings, "general.defaultExportDirectory", "")}
                    readOnly
                    aria-label={translate("settings.defaultExportLocation")}
                  />
                  <button className="settings-action" type="button" onClick={() => void browseExportDirectory()}>{translate("settings.select")}</button>
                </div>
              </div>
              <Checkbox
                className="settings-check"
                checked={Boolean(settings["general.autoOpenOutputFolder"] ?? false)}
                onChange={(checked) => void persist({ "general.autoOpenOutputFolder": checked })}
              >
                {translate("settings.openOutputFolder")}
              </Checkbox>
              {updatesSupported === true && (
                <>
                  <Checkbox
                    className="settings-check"
                    checked={Boolean(settings["updates.checkOnStartup"] ?? true)}
                    onChange={(checked) => void persist({ "updates.checkOnStartup": checked })}
                  >
                    {translate("settings.checkOnStartup")}
                  </Checkbox>
                  <div className="settings-field">
                    <span>{translate("settings.update")}</span>
                    <button className="settings-action settings-action--quiet" type="button" disabled={updateChecking || updateInstalling} onClick={() => void checkUpdates()}>
                      {updateChecking ? translate("settings.checking") : translate("settings.checkNow")}
                    </button>
                  </div>
                </>
              )}
              {updatesSupported === false && (
                <div className="settings-field">
                  <span>{translate("settings.update")}</span>
                  <span className="settings-field__hint">{translate("settings.reinstallToUpdate")}</span>
                </div>
              )}
            </div>
          )}

          {category === "ai" && (
            <div className="settings-fields">
              <div className="settings-field">
                <span>{translate("settings.provider")}</span>
                <Select
                  aria-label={translate("settings.provider")}
                  value={ai?.provider ?? "chatgpt"}
                  disabled={busy || loginPending || plamoDownloading || !ai}
                  onChange={(provider) => void selectProvider(provider as AiProviderId)}
                  options={PROVIDERS.map(([value, labelKey]) => ({ value, label: translate(labelKey) }))}
                />
              </div>
              {ai?.provider === "chatgpt" ? (
                ai.configured ? (
                  <div className="settings-field">
                    <span>{translate("settings.account")}</span>
                    <span>{ai.accountLabel ?? translate("settings.signedIn")}</span>
                    <button className="settings-action settings-action--quiet" type="button" disabled={busy || loginPending} onClick={() => void logout()}>{translate("settings.logout")}</button>
                  </div>
                ) : (
                  <div className="settings-field">
                    <button className="settings-action" type="button" disabled={busy || loginPending} onClick={() => void beginChatgptLogin()}>{translate("settings.chatgptLogin")}</button>
                    {loginPending && (
                      <>
                        <output className="settings-field__hint" aria-live="polite">{translate("settings.browserLogin")}</output>
                        <button className="settings-action settings-action--quiet" type="button" onClick={() => void cancelLogin()}>{translate("settings.cancel")}</button>
                      </>
                    )}
                  </div>
                )
              ) : (
                // PLaMo はローカル実行で認証情報を持たないため、APIキー欄を出さない。
                ai?.provider !== "plamo" && !(ai?.provider === "ollama" || ai?.provider === "lm-studio") && (
                  <label className="settings-field">
                  {translate("settings.apiKey")}
                  <input
                    type="password"
                    autoComplete="new-password"
                    value={apiKey}
                    onChange={(event) => setApiKey(event.currentTarget.value)}
                    placeholder={ai?.configured ? translate("settings.changeApiKeyPlaceholder") : ""}
                  />
                  {ai?.configured && <span className="settings-field__hint">{translate("settings.configured")}</span>}
                  </label>
                )
              )}
              {ai?.provider === "plamo" ? (
                <PlamoSection
                  settings={ai}
                  status={plamoStatus}
                  statusFailed={plamoStatusFailed}
                  busy={busy || loginPending}
                  targetLocale={targetLocale}
                  onSettingsChange={(patch) => setAi((current) => current ? { ...current, ...patch } : current)}
                  onSaveSelection={savePlamoSelection}
                  onRefresh={refreshPlamo}
                  onDownloadingChange={setPlamoDownloading}
                />
              ) : ai?.provider !== "deepl" && (
                  <div className="settings-field">
                    <label htmlFor="ai-model">{translate("settings.model")}</label>
                    <Combobox
                      id="ai-model"
                      options={modelOptions}
                      value={ai?.model ?? ""}
                      aria-describedby={modelError ? "ai-model-error" : undefined}
                      onChange={(model) => {
                        setModelError(null);
                        setAi((current) => current ? { ...current, model } : current);
                      }}
                    />
                    {modelError && <span id="ai-model-error" className="settings-field__hint settings-field__hint--error" role="alert">{modelError}</span>}
                  </div>
              )}
              {ai?.provider === "openai-compatible" && (
                <label className="settings-field">
                  {translate("settings.connectionUrl")}
                  <input
                    type="url"
                    value={ai.baseUrl}
                    onChange={(event) => {
                      const baseUrl = event.currentTarget.value;
                      setAi((current) => current ? { ...current, baseUrl } : current);
                    }}
                  />
                  <span className="settings-field__hint">{translate("settings.httpLoopbackHelp")}</span>
                </label>
              )}
              <Disclosure className="settings-advanced" title={translate("settings.advancedSettings")}>
                <div className="settings-fields">
                  {/* PLaMo はバックエンドが送信件数を無視するため、意味のない欄を出さない。 */}
                  {ai?.provider !== "plamo" && (
                    <label className="settings-field" htmlFor="ai-batch-size">
                      {translate("settings.requestsPerBatch")}
                      <NumberInput
                        id="ai-batch-size"
                        min={1}
                        max={50}
                        value={ai?.batchSize ?? 50}
                        onChange={(event) => {
                          const batchSize = Number(event.currentTarget.value);
                          setAi((current) => current ? { ...current, batchSize } : current);
                        }}
                      />
                    </label>
                  )}
                  <div className="settings-field">
                    <span>{translate("settings.parallelRequests")}</span>
                    <Select
                      aria-label={translate("settings.parallelRequests")}
                      value={String(ai?.concurrency ?? 2)}
                      onChange={(value) => setAi((current) => current ? { ...current, concurrency: Number(value) } : current)}
                      options={[1, 2, 3, 4].map((value) => ({ value: String(value), label: String(value) }))}
                    />
                  </div>
                  {/* PLaMo は指示追従モデルではないため、追加指示の欄を出さない。 */}
                  {ai?.provider !== "deepl" && ai?.provider !== "plamo" && (
                    <label className="settings-field">
                    {translate("settings.additionalInstructions")}
                      <textarea
                        rows={5}
                        value={ai?.customPrompt ?? ""}
                        onChange={(event) => {
                          const customPrompt = event.currentTarget.value;
                          setAi((current) => current ? { ...current, customPrompt } : current);
                        }}
                      />
                    </label>
                  )}
                </div>
              </Disclosure>
              <div className="settings-actions">
                <button className="settings-action" type="button" disabled={busy || loginPending || !ai} onClick={() => void saveProvider()}>{translate("settings.save")}</button>
                <button className="settings-action" type="button" disabled={busy || loginPending || !ai?.configured} onClick={() => void testProvider()}>{translate("settings.testConnection")}</button>
                {/* PLaMo は認証情報を持たず delete_ai_provider を使わないため、削除はモデル単位で行う。 */}
                {ai?.configured && ai.provider !== "chatgpt" && ai.provider !== "plamo" && (
                  <button className="settings-action settings-action--quiet" type="button" disabled={busy} onClick={() => void removeProvider()}>{translate("settings.delete")}</button>
                )}
              </div>
            </div>
          )}

          {category === "editor" && (
            <div className="settings-fields">
              <label className="settings-field" htmlFor="editor-font-size">
                {translate("settings.fontSize")}
                <NumberInput
                  id="editor-font-size"
                  min={10}
                  max={20}
                  key={numberSetting(settings, "editor.fontSize", SETTING_DEFAULTS["editor.fontSize"] as number)}
                  defaultValue={numberSetting(settings, "editor.fontSize", SETTING_DEFAULTS["editor.fontSize"] as number)}
                  onBlur={(event) => {
                    const value = Number(event.currentTarget.value);
                    if (Number.isInteger(value) && value >= 10 && value <= 20) void persist({ "editor.fontSize": value });
                  }}
                />
              </label>
              <div className="settings-field">
                <span>{translate("settings.lineHeight")}</span>
                <Select
                  aria-label={translate("settings.lineHeight")}
                  value={stringSetting(settings, "editor.rowDensity", "standard")}
                  onChange={(value) => void persist({ "editor.rowDensity": value })}
                  options={[
                    { value: "compact", label: translate("settings.densityCompact") },
                    { value: "standard", label: translate("settings.densityStandard") },
                    { value: "spacious", label: translate("settings.densitySpacious") },
                  ]}
                />
              </div>
              <div className="settings-field">
                <span>{translate("settings.modTabPosition")}</span>
                <Select
                  aria-label={translate("settings.modTabPosition")}
                  value={stringSetting(settings, "editor.modTabPosition", "top")}
                  onChange={(value) => void persist({ "editor.modTabPosition": value })}
                  options={[
                    { value: "top", label: translate("settings.positionTop") },
                    { value: "left", label: translate("settings.positionLeft") },
                  ]}
                />
              </div>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.wrapSource"] ?? false)} onChange={(checked) => void persist({ "editor.wrapSource": checked })}>
                {translate("settings.wrapSource")}
              </Checkbox>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.wrapTarget"] ?? false)} onChange={(checked) => void persist({ "editor.wrapTarget": checked })}>
                {translate("settings.wrapTranslation")}
              </Checkbox>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.hideInspectorPreview"] ?? false)} onChange={(checked) => void persist({ "editor.hideInspectorPreview": checked })}>
                {translate("settings.hideInspectorPreview")}
              </Checkbox>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.confirmBatchOverwrite"] ?? true)} onChange={(checked) => void persist({ "editor.confirmBatchOverwrite": checked })}>
                {translate("settings.confirmTranslationOverwrite")}
              </Checkbox>
            </div>
          )}

          {category === "appearance" && (
            <RadioGroup
              className="settings-radio-group"
              legend={translate("settings.theme")}
              legendClassName="settings-radio-group__legend"
              name="theme"
              value={String(settings["appearance.theme"] ?? "")}
              onChange={(value) => void persist({ "appearance.theme": value })}
              options={[
                { value: "light", label: translate("settings.themeLight"), id: "theme-light" },
                { value: "dark", label: translate("settings.themeDark"), id: "theme-dark" },
                { value: "system", label: translate("settings.themeSystem"), id: "theme-system" },
              ]}
            />
          )}

          {category === "advanced" && (
            <div className="settings-fields">
              <div className="settings-field">
                <span>{translate("settings.cache")}</span>
                <button className="settings-action settings-action--quiet" type="button" onClick={() => void clearAppCache()}>{translate("settings.delete")}</button>
              </div>
              <div className="settings-field">
                <span>{translate("settings.projectLocation")}</span>
                <code className="settings-location selectable">{appInfo ? `${appInfo.dataDirectory}/projects` : "—"}</code>
              </div>
              <div className="settings-field">
                <Checkbox className="settings-check" checked={Boolean(settings["advanced.debugLogs"] ?? false)} onChange={(checked) => void persist({ "advanced.debugLogs": checked })}>
                  {translate("settings.debugLogs")}
                </Checkbox>
                <span className="settings-field__hint">{translate("settings.restartToApply")}</span>
              </div>
              <MinecraftJarSection />
            </div>
          )}

          {category === "logs" && <LogViewer />}

          {error && <p className="settings-message settings-message--error selectable" role="alert">{error}</p>}
          {message && <p className="settings-message" aria-live="polite">{message}</p>}
        </section>
      </div>
      <footer className="settings-actions">
        <SecondaryButton onClick={() => void closeSettings()}>{translate("settings.back")}</SecondaryButton>
      </footer>
    </section>
  );
}
