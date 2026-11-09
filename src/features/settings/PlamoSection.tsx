import { useCallback, useEffect, useState } from "react";

import { Dialog, Select } from "../../components/Controls";
import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { ProgressBar } from "../../components/progress/ProgressBar";
import { useDialogConfirmation } from "../../components/useDialogConfirmation";
import { formatNumber, useT, useUiLanguage } from "../../i18n";
import { targetLanguageName } from "../../i18n/minecraftLanguages";
import {
  AppCommandError,
  acceptPlamoLicense,
  cancelPlamoDownload,
  deletePlamoModel,
  downloadPlamoFiles,
  openPlamoLicense,
} from "../../lib/tauri";
import type { AiSettingsDto, PlamoDevice, PlamoStatusDto } from "../../types/ai";

interface PlamoSectionProps {
  settings: AiSettingsDto;
  status: PlamoStatusDto | null;
  statusFailed: boolean;
  busy: boolean;
  targetLocale: string;
  onSettingsChange: (patch: Partial<AiSettingsDto>) => void;
  onSaveSelection: () => Promise<boolean>;
  onRefresh: () => Promise<void>;
  // ダウンロード中はプロバイダーを切り替えられないよう、親へ実行状況を伝える。
  onDownloadingChange: (downloading: boolean) => void;
}

interface DownloadProgress {
  phase: "runtime" | "model";
  receivedBytes: number;
  totalBytes: number;
}

function formatSize(bytes: number) {
  // ランタイムとモデルを同じ物差しで比べられるよう、常に GB で示す。
  return `${formatNumber(bytes / 1024 ** 3)} GB`;
}

function failureMessage(cause: unknown, fallback: string) {
  return cause instanceof AppCommandError ? cause.appError.userMessage : fallback;
}

export function PlamoSection({
  settings,
  status,
  statusFailed,
  busy,
  targetLocale,
  onSettingsChange,
  onSaveSelection,
  onRefresh,
  onDownloadingChange,
}: PlamoSectionProps) {
  const translate = useT();
  const uiLanguage = useUiLanguage();
  const { requestConfirmation, dialog } = useDialogConfirmation();
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [licenseDialogOpen, setLicenseDialogOpen] = useState(false);

  const device: PlamoDevice = settings.device ?? "gpu";
  // 保存済みのモデルが一覧に無い場合でも操作不能にならないよう、既定の 2B に寄せる。
  const selectedModel = status?.models.find((model) => model.id === settings.model)
    ?? status?.models.find((model) => model.id === "plamo-3-translate-2b")
    ?? status?.models[0];
  // 選択中のモデルと、そのデバイス用ランタイムのうち、まだ無いものだけを合計する。
  const missingBytes = status && selectedModel
    ? (status.runtimeReady[device] ? 0 : status.runtimeSizeBytes[device])
      + (selectedModel.downloaded ? 0 : selectedModel.sizeBytes)
    : 0;
  const targetLocaleSupported = status
    ? status.supportedTargetLocales.includes(targetLocale.toLowerCase())
    : true;
  // 画面を開き直した時点で進行中のダウンロードは、この画面では進捗値を受け取れない。
  const activeDownload = downloading || (status?.downloading ?? false);

  useEffect(() => {
    onDownloadingChange(activeDownload);
  }, [activeDownload, onDownloadingChange]);

  // 開いた時点で既に走っていたダウンロードにはイベントチャネルが無いため、状態を取り直して完了を検知する。
  // この画面から開始した分はイベントで追えるので、選択中の値を上書きしないよう問い合わせない。
  useEffect(() => {
    if (!status?.downloading || downloading) return;
    const timer = window.setInterval(() => { void onRefresh(); }, 2000);
    return () => window.clearInterval(timer);
  }, [status?.downloading, downloading, onRefresh]);

  const openLicense = useCallback(async () => {
    try {
      await openPlamoLicense(uiLanguage === "ja" ? "ja" : "en");
    } catch (cause: unknown) {
      setError(failureMessage(cause, translate("settings.genericError")));
    }
  }, [translate, uiLanguage]);

  const startDownload = async () => {
    if (!selectedModel) return;
    setError(null);
    // 選択中のモデル・デバイスを先に保存する。保存しないまま取得すると、完了後の再取得で
    // 保存済みの値に戻り、ダウンロードした選択が失われる。
    if (!(await onSaveSelection())) return;
    setProgress(null);
    setDownloading(true);
    try {
      await downloadPlamoFiles(selectedModel.id, device, (event) => {
        if (event.kind === "progress") {
          setProgress({ phase: event.phase, receivedBytes: event.receivedBytes, totalBytes: event.totalBytes });
          return;
        }
        if (event.kind === "failed") setError(event.error.userMessage);
        setDownloading(false);
        setProgress(null);
      });
    } catch (cause: unknown) {
      setError(failureMessage(cause, translate("settings.genericError")));
    } finally {
      setDownloading(false);
      setProgress(null);
      // 失敗しても部分ダウンロードは残るため、取り直して残り容量を正しく出す。
      await onRefresh();
    }
  };

  const beginDownload = async () => {
    if (!status) return;
    if (status.licenseAccepted) {
      await startDownload();
      return;
    }
    setLicenseDialogOpen(true);
  };

  const acceptLicenseAndDownload = async () => {
    setLicenseDialogOpen(false);
    try {
      await acceptPlamoLicense();
    } catch (cause: unknown) {
      setError(failureMessage(cause, translate("settings.genericError")));
      return;
    }
    await startDownload();
  };

  const cancelDownload = async () => {
    try {
      await cancelPlamoDownload();
      // 画面を開く前に始まったダウンロードにはチャネルが無いため、状態を取り直して表示を戻す。
      setDownloading(false);
      setProgress(null);
      await onRefresh();
    } catch (cause: unknown) {
      setError(failureMessage(cause, translate("settings.genericError")));
    }
  };

  const removeModel = async () => {
    if (!selectedModel) return;
    const accepted = await requestConfirmation({
      title: translate("settings.plamoDeleteTitle"),
      message: translate("settings.plamoDeleteMessage", { model: selectedModel.parameterLabel }),
      confirmLabel: translate("settings.delete"),
      destructive: true,
    });
    if (!accepted) return;
    setError(null);
    try {
      await deletePlamoModel(selectedModel.id);
      await onRefresh();
    } catch (cause: unknown) {
      setError(failureMessage(cause, translate("settings.genericError")));
    }
  };

  const modelOptions = (status?.models ?? []).map((model) => ({
    value: model.id,
    label: [
      model.parameterLabel,
      formatSize(model.sizeBytes),
      model.downloaded ? translate("settings.plamoDownloaded") : null,
    ].filter((part) => part !== null).join(" · "),
  }));

  return (
    <>
      {!status && (
        statusFailed
          ? <p className="settings-field__hint settings-field__hint--error" role="alert">{translate("settings.genericError")}</p>
          : <output className="settings-field__hint" aria-live="polite">{translate("settings.loading")}</output>
      )}
      {status && (
        <>
          <div className="settings-field">
            <span>{translate("settings.model")}</span>
            <Select
              aria-label={translate("settings.model")}
              value={selectedModel?.id ?? settings.model}
              disabled={busy || activeDownload}
              onChange={(model) => onSettingsChange({ model })}
              options={modelOptions}
            />
          </div>
          <div className="settings-field">
            <span>{translate("settings.plamoDevice")}</span>
            <Select
              aria-label={translate("settings.plamoDevice")}
              value={device}
              disabled={busy || activeDownload}
              onChange={(value) => onSettingsChange({ device: value as PlamoDevice })}
              options={[
                { value: "gpu", label: translate("settings.plamoDeviceGpu") },
                { value: "cpu", label: translate("settings.plamoDeviceCpu") },
              ]}
            />
            <span className="settings-field__hint">{translate("settings.plamoDeviceHint")}</span>
            {/* 実測で 8B の CPU 実行は 0.28 件/秒（GPU は 2.8 件/秒）。待ち時間の見積もりが
                立たないまま選ばれるのを避けるため、この組み合わせのときだけ短く添える。 */}
            {device === "cpu" && selectedModel?.id === "plamo-3-translate-8b" && (
              <span className="settings-field__hint">{translate("settings.plamoCpuSlowHint")}</span>
            )}
          </div>
          {!targetLocaleSupported && (
            <p className="settings-field__hint">
              {translate("settings.plamoTargetUnsupported", { language: targetLanguageName(targetLocale) })}
            </p>
          )}
          {activeDownload ? (
            <div className="settings-field">
              {progress && progress.totalBytes > 0
                ? (
                  <>
                    <ProgressBar
                      value={(progress.receivedBytes / progress.totalBytes) * 100}
                      label={translate("settings.plamoDownloadProgressAria")}
                    />
                    <span className="settings-field__hint">
                      {translate("settings.plamoProgress", {
                        phase: translate(progress.phase === "runtime" ? "settings.plamoPhaseRuntime" : "settings.plamoPhaseModel"),
                        received: formatSize(progress.receivedBytes),
                        total: formatSize(progress.totalBytes),
                      })}
                    </span>
                  </>
                )
                : (
                  // 再開した画面では進捗値が無いため、不確定進捗として示す
                  // （LoadingScreen と同じく ProgressBar の確定値契約には持ち込まない）。
                  <progress className="progress-bar" max={100} aria-label={translate("settings.plamoDownloadProgressAria")} />
                )}
              <div className="settings-actions">
                <button className="settings-action" type="button" onClick={() => void cancelDownload()}>
                  {translate("settings.cancel")}
                </button>
              </div>
            </div>
          ) : (
            <div className="settings-actions">
              {missingBytes > 0 && (
                <button className="settings-action" type="button" disabled={busy} onClick={() => void beginDownload()}>
                  {translate("settings.plamoDownload", { size: formatSize(missingBytes) })}
                </button>
              )}
              {selectedModel?.downloaded && (
                <button className="settings-action" type="button" disabled={busy} onClick={() => void removeModel()}>
                  {translate("settings.delete")}
                </button>
              )}
            </div>
          )}
        </>
      )}
      {error && <p className="settings-field__hint settings-field__hint--error" role="alert">{error}</p>}
      {/* PLaMo Community License が "Built with PLaMo" の表示を求めるため、状態に関わらず常に出す。 */}
      <div className="settings-actions">
        <span className="settings-field__hint">{translate("settings.plamoAttribution")}</span>
        <button className="settings-action settings-action--quiet" type="button" onClick={() => void openLicense()}>
          {translate("settings.plamoLicenseOpen")}
        </button>
      </div>
      <Dialog open={licenseDialogOpen} title={translate("settings.plamoLicenseTitle")} onClose={() => setLicenseDialogOpen(false)}>
        <p>{translate("settings.plamoLicenseSummary")}</p>
        <div className="dialog-confirmation__actions">
          <SecondaryButton onClick={() => setLicenseDialogOpen(false)}>{translate("settings.cancel")}</SecondaryButton>
          <SecondaryButton onClick={() => void openLicense()}>{translate("settings.plamoLicenseOpen")}</SecondaryButton>
          <PrimaryButton onClick={() => void acceptLicenseAndDownload()}>{translate("settings.plamoLicenseAccept")}</PrimaryButton>
        </div>
      </Dialog>
      {dialog}
    </>
  );
}
