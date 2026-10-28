import { useCallback, useEffect, useRef, useState } from "react";

import { AppCommandError, cancelChatgptLogin, clearCache, deleteAiProvider, getAiSettings, getAppInfo, getRecentLog, listAiModels, logoutChatgpt, openLogFolder, saveAiProvider, startChatgptLogin, testAiProvider } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { openFileDialog } from "../../lib/dialogs";
import { isImeCompositionKeyDown } from "../../lib/keyboard";
import { Checkbox, Combobox, Disclosure, NumberInput, RadioGroup, Select } from "../../components/Controls";
import { SecondaryButton } from "../../components/button/Buttons";
import { useAppStore } from "../../stores/appStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import { checkForUpdate, loadUpdateSupport, useUpdaterStore } from "../updater/updater";
import type { AiProviderId, AiSettingsDto } from "../../types/ai";
import type { AppInfoDto } from "../../types/settings";
import { MinecraftJarSection } from "./MinecraftJarSection";
import "./SettingsScreen.css";

type SettingsCategory = "general" | "ai" | "editor" | "appearance" | "advanced" | "logs";

const CATEGORIES: Array<[SettingsCategory, string]> = [
  ["general", "一般"],
  ["ai", "AI翻訳"],
  ["editor", "エディター"],
  ["appearance", "表示"],
  ["advanced", "詳細"],
  ["logs", "ログ"],
];

// 既定の ChatGPT を先頭に置く（既定値は settingsStore と commands/ai.rs の provider_from_settings）。
const PROVIDERS: Array<[AiProviderId, string]> = [
  ["chatgpt", "ChatGPT（サブスクリプション）"],
  ["openai", "OpenAI"],
  ["anthropic", "Claude（Anthropic）"],
  ["gemini", "Gemini"],
  ["openrouter", "OpenRouter"],
  ["deepseek", "DeepSeek"],
  ["groq", "Groq"],
  ["ollama", "Ollama"],
  ["lm-studio", "LM Studio"],
  ["openai-compatible", "OpenAI 互換（カスタム）"],
  ["deepl", "DeepL"],
];

function aiSaveConfig(settings: AiSettingsDto) {
  return {
    provider: settings.provider,
    model: settings.model,
    baseUrl: settings.baseUrl,
    batchSize: settings.batchSize,
    concurrency: settings.concurrency,
    customPrompt: settings.customPrompt,
  };
}

function errorMessage(error: unknown) {
  void logFrontendError(error, "settings screen");
  return error instanceof AppCommandError ? error.message : "処理に失敗しました。もう一度お試しください。";
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
      setMessage("コピーしました");
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
          再読み込み
        </button>
        <button className="settings-action" type="button" disabled={!content} onClick={() => void copy()}>
          コピー
        </button>
        <button className="settings-action" type="button" onClick={() => void openFolder()}>
          ログフォルダを開く
        </button>
      </div>
      {loading && <output className="settings-field__hint" aria-live="polite">読み込み中…</output>}
      {!loading && !content && <p className="settings-field__hint">ログはありません</p>}
      {content && <pre className="settings-log selectable" aria-label="ログ">{content}</pre>}
      {error && <p className="settings-message settings-message--error selectable" role="alert">{error}</p>}
      {message && <p className="settings-message" aria-live="polite">{message}</p>}
    </div>
  );
}

export function SettingsScreen() {
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
  const modelProvider = ai?.provider;
  const modelBaseUrl = ai?.baseUrl;
  const modelProviderConfigured = ai?.configured ?? false;

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
        setModelError("モデル一覧を取得できません。モデル名を入力してください。");
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
      setModelError("モデルを入力してください。");
      return;
    }
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const saved = await saveAiProvider(aiSaveConfig(ai), apiKey || undefined);
      setAi(saved);
      setApiKey("");
      setMessage("保存しました");
      await loadSettings();
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
      setMessage("ログインしました");
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
      setMessage("ログアウトしました");
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
      setMessage("削除しました");
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
      setMessage("接続できました");
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
      setMessage("キャッシュを削除しました");
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
        ? "開発ビルドでは更新を確認できません。"
        : update ? `新しいバージョン ${update.version} があります` : "最新です");
    } catch (cause: unknown) {
      console.warn("手動の更新確認に失敗しました。", cause);
      void logFrontendError(cause, "manual update check");
      setError("更新を確認できませんでした。");
    } finally {
      setUpdateChecking(false);
    }
  };

  const categoryLabel = CATEGORIES.find(([key]) => key === category)?.[1] ?? "一般";

  return (
    <section className="settings-screen" aria-labelledby="settings-title">
      <h1 className="page-title" id="settings-title">設定</h1>
      <div className="settings-layout">
        <nav className="settings-nav" aria-label="設定カテゴリ">
          {CATEGORIES.map(([key, label]) => (
            <button
              className={`settings-nav__item${category === key ? " settings-nav__item--selected" : ""}`}
              type="button"
              aria-current={category === key ? "page" : undefined}
              key={key}
              onClick={() => useAppStore.setState({ settingsCategory: key })}
            >
              {label}
            </button>
          ))}
        </nav>
        <section className="settings-content" aria-labelledby="settings-category-title">
          <h2 id="settings-category-title">{categoryLabel}</h2>
          {category === "general" && (
            <div className="settings-fields">
              <label className="settings-field" htmlFor="recent-project-count">
                最近のプロジェクト数
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
                <span>既定の書き出し先</span>
                <div className="settings-path-field">
                  <input
                    value={stringSetting(settings, "general.defaultExportDirectory", "")}
                    readOnly
                    aria-label="既定の書き出し先"
                  />
                  <button className="settings-action" type="button" onClick={() => void browseExportDirectory()}>選択</button>
                </div>
              </div>
              <Checkbox
                className="settings-check"
                checked={Boolean(settings["general.autoOpenOutputFolder"] ?? false)}
                onChange={(checked) => void persist({ "general.autoOpenOutputFolder": checked })}
              >
                書き出し後にフォルダーを開く
              </Checkbox>
              {updatesSupported === true && (
                <>
                  <Checkbox
                    className="settings-check"
                    checked={Boolean(settings["updates.checkOnStartup"] ?? true)}
                    onChange={(checked) => void persist({ "updates.checkOnStartup": checked })}
                  >
                    起動時に更新を確認
                  </Checkbox>
                  <div className="settings-field">
                    <span>更新</span>
                    <button className="settings-action settings-action--quiet" type="button" disabled={updateChecking || updateInstalling} onClick={() => void checkUpdates()}>
                      {updateChecking ? "確認中…" : "今すぐ確認"}
                    </button>
                  </div>
                </>
              )}
              {updatesSupported === false && (
                <div className="settings-field">
                  <span>更新</span>
                  <span className="settings-field__hint">パッケージを再インストールして更新してください</span>
                </div>
              )}
            </div>
          )}

          {category === "ai" && (
            <div className="settings-fields">
              <div className="settings-field">
                <span>プロバイダー</span>
                <Select
                  aria-label="プロバイダー"
                  value={ai?.provider ?? "chatgpt"}
                  disabled={busy || loginPending || !ai}
                  onChange={(provider) => void selectProvider(provider as AiProviderId)}
                  options={PROVIDERS.map(([value, label]) => ({ value, label }))}
                />
              </div>
              {ai?.provider === "chatgpt" ? (
                ai.configured ? (
                  <div className="settings-field">
                    <span>アカウント</span>
                    <span>{ai.accountLabel ?? "ログイン済み"}</span>
                    <button className="settings-action settings-action--quiet" type="button" disabled={busy || loginPending} onClick={() => void logout()}>ログアウト</button>
                  </div>
                ) : (
                  <div className="settings-field">
                    <button className="settings-action" type="button" disabled={busy || loginPending} onClick={() => void beginChatgptLogin()}>ChatGPTでログイン</button>
                    {loginPending && (
                      <>
                        <output className="settings-field__hint" aria-live="polite">ブラウザでログインしてください</output>
                        <button className="settings-action settings-action--quiet" type="button" onClick={() => void cancelLogin()}>キャンセル</button>
                      </>
                    )}
                  </div>
                )
              ) : (
                !(ai?.provider === "ollama" || ai?.provider === "lm-studio") && (
                  <label className="settings-field">
                  APIキー
                  <input
                    type="password"
                    autoComplete="new-password"
                    value={apiKey}
                    onChange={(event) => setApiKey(event.currentTarget.value)}
                    placeholder={ai?.configured ? "変更する場合は入力" : ""}
                  />
                  {ai?.configured && <span className="settings-field__hint">設定済み</span>}
                  </label>
                )
              )}
              {ai?.provider !== "deepl" && (
                  <div className="settings-field">
                    <label htmlFor="ai-model">モデル</label>
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
                  接続先URL
                  <input
                    type="url"
                    value={ai.baseUrl}
                    onChange={(event) => {
                      const baseUrl = event.currentTarget.value;
                      setAi((current) => current ? { ...current, baseUrl } : current);
                    }}
                  />
                  <span className="settings-field__hint">HTTP は localhost または loopback のみ利用できます。</span>
                </label>
              )}
              <Disclosure className="settings-advanced" title="詳細設定">
                <div className="settings-fields">
                  <label className="settings-field" htmlFor="ai-batch-size">
                    1回の送信件数
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
                  <div className="settings-field">
                    <span>並列数</span>
                    <Select
                      aria-label="並列数"
                      value={String(ai?.concurrency ?? 2)}
                      onChange={(value) => setAi((current) => current ? { ...current, concurrency: Number(value) } : current)}
                      options={[1, 2, 3, 4].map((value) => ({ value: String(value), label: String(value) }))}
                    />
                  </div>
                  {ai?.provider !== "deepl" && (
                    <label className="settings-field">
                    追加指示
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
                <button className="settings-action" type="button" disabled={busy || loginPending || !ai} onClick={() => void saveProvider()}>保存</button>
                <button className="settings-action" type="button" disabled={busy || loginPending || !ai?.configured} onClick={() => void testProvider()}>接続テスト</button>
                {ai?.configured && ai.provider !== "chatgpt" && (
                  <button className="settings-action settings-action--quiet" type="button" disabled={busy} onClick={() => void removeProvider()}>削除</button>
                )}
              </div>
            </div>
          )}

          {category === "editor" && (
            <div className="settings-fields">
              <label className="settings-field" htmlFor="editor-font-size">
                文字サイズ
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
                <span>行の高さ</span>
                <Select
                  aria-label="行の高さ"
                  value={stringSetting(settings, "editor.rowDensity", "standard")}
                  onChange={(value) => void persist({ "editor.rowDensity": value })}
                  options={[
                    { value: "compact", label: "コンパクト" },
                    { value: "standard", label: "標準" },
                    { value: "spacious", label: "広め" },
                  ]}
                />
              </div>
              <div className="settings-field">
                <span>modタブの位置</span>
                <Select
                  aria-label="modタブの位置"
                  value={stringSetting(settings, "editor.modTabPosition", "top")}
                  onChange={(value) => void persist({ "editor.modTabPosition": value })}
                  options={[
                    { value: "top", label: "上" },
                    { value: "left", label: "左" },
                  ]}
                />
              </div>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.wrapSource"] ?? false)} onChange={(checked) => void persist({ "editor.wrapSource": checked })}>
                原文を折り返す
              </Checkbox>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.wrapTarget"] ?? false)} onChange={(checked) => void persist({ "editor.wrapTarget": checked })}>
                翻訳文を折り返す
              </Checkbox>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.hideInspectorPreview"] ?? false)} onChange={(checked) => void persist({ "editor.hideInspectorPreview": checked })}>
                詳細のプレビューを非表示
              </Checkbox>
              <Checkbox className="settings-check" checked={Boolean(settings["editor.confirmBatchOverwrite"] ?? true)} onChange={(checked) => void persist({ "editor.confirmBatchOverwrite": checked })}>
                既存の翻訳を上書きする前に確認
              </Checkbox>
            </div>
          )}

          {category === "appearance" && (
            <RadioGroup
              className="settings-radio-group"
              legend="テーマ"
              legendClassName="settings-radio-group__legend"
              name="theme"
              value={String(settings["appearance.theme"] ?? "")}
              onChange={(value) => void persist({ "appearance.theme": value })}
              options={[
                { value: "light", label: "ライト", id: "theme-light" },
                { value: "dark", label: "ダーク", id: "theme-dark" },
                { value: "system", label: "システムに合わせる", id: "theme-system" },
              ]}
            />
          )}

          {category === "advanced" && (
            <div className="settings-fields">
              <div className="settings-field">
                <span>キャッシュ</span>
                <button className="settings-action settings-action--quiet" type="button" onClick={() => void clearAppCache()}>削除</button>
              </div>
              <div className="settings-field">
                <span>プロジェクトの保存先</span>
                <code className="settings-location selectable">{appInfo ? `${appInfo.dataDirectory}/projects` : "—"}</code>
              </div>
              <div className="settings-field">
                <Checkbox className="settings-check" checked={Boolean(settings["advanced.debugLogs"] ?? false)} onChange={(checked) => void persist({ "advanced.debugLogs": checked })}>
                  デバッグログ
                </Checkbox>
                <span className="settings-field__hint">変更は再起動後に反映されます。</span>
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
        <SecondaryButton onClick={() => void closeSettings()}>戻る</SecondaryButton>
      </footer>
    </section>
  );
}
