import { useEffect, useRef, useState } from "react";

import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { AppCommandError, openOutputFolder } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { useAppStore } from "../../stores/appStore";
import { getExportRunDetails } from "./completionData";
import { displayPath } from "./pathDisplay";
import "./ExportCompletionScreen.css";

const FORMAT_LABELS = {
  resourcePack: "リソースパック",
  modifiedJar: "Mod JAR",
  languageFile: "言語ファイル",
  modpackFiles: "Modpack ファイル",
  shaderpackZip: "日本語化したシェーダーパック",
} as const;

function errorMessage(error: unknown) {
  void logFrontendError(error, "export completion screen");
  return error instanceof AppCommandError ? error.message : "フォルダを開けませんでした。";
}

function fileName(path: string) {
  return path.split(/[\\/]/).at(-1) || "—";
}

function directoryName(path: string) {
  const separator = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return separator >= 0 ? path.slice(0, separator) : "—";
}

function statusLabel(status: string) {
  switch (status) {
    case "success": return "完了";
    case "skipped": return "スキップ";
    default: return "失敗";
  }
}

export function ExportCompletionScreen() {
  const completion = useAppStore((state) => state.exportCompletion);
  const goTo = useAppStore((state) => state.goTo);
  const runDetails = getExportRunDetails();
  const [error, setError] = useState<string | null>(null);
  const autoOpenStarted = useRef(false);
  const outputPaths = runDetails?.outputPaths ?? (completion?.outputPath ? [completion.outputPath] : []);
  const firstOutput = runDetails?.outputPaths[0] ?? completion?.outputPath ?? "";

  const showOutputFolder = async () => {
    if (!firstOutput) return;
    setError(null);
    try {
      await openOutputFolder(firstOutput);
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  };

  useEffect(() => {
    if (!completion?.autoOpenOutputFolder || autoOpenStarted.current || !firstOutput) return;
    autoOpenStarted.current = true;
    setError(null);
    void openOutputFolder(firstOutput).catch((cause: unknown) => setError(errorMessage(cause)));
  }, [completion?.autoOpenOutputFolder, firstOutput]);

  if (!completion) {
    return <section className="focus-page export-completion" aria-live="polite">書き出し情報がありません。</section>;
  }

  let instruction: string;
  switch (completion.kind) {
    case "resourcePack":
      instruction = "Minecraft のリソースパック設定で有効にしてください";
      break;
    case "modifiedJar":
      instruction = runDetails?.inPlaceReplacement
        ? "元のJARは書き出し画面から戻せます"
        : "元のJARは変更していません";
      break;
    case "languageFile":
      instruction = completion.sourceKind === "shaderpack"
        ? "配置先: shaders/lang/"
        : `配置先: assets/${completion.namespace}/lang/`;
      break;
    case "modpackFiles":
      instruction = runDetails?.directInstall
        ? "配置したファイルは書き出し画面から元に戻せます"
        : "ZIP を展開してインスタンスへ配置してください";
      break;
    case "shaderpackZip":
      instruction = runDetails?.directInstall
        ? "配置した ZIP は書き出し画面から元に戻せます。ゲーム内のシェーダー選択で選び直してください"
        : "ゲーム内のシェーダー選択で選び直してください";
      break;
  }

  const successfulCount = runDetails?.modResults.filter((result) => result.status === "success").length ?? 0;
  const hasFailedMod = runDetails?.modResults.some((result) => result.status !== "success") ?? false;
  const allFailed = hasFailedMod && successfulCount === 0 && outputPaths.length === 0;

  return (
    <section className="focus-page export-completion" aria-labelledby="export-completion-title">
      <h1 className="page-title" id="export-completion-title">
        {allFailed ? "書き出せませんでした" : "書き出しました"}
      </h1>
      <dl className="export-completion__details">
        <div>
          <dt>形式</dt>
          <dd>{FORMAT_LABELS[completion.kind]}</dd>
        </div>
        {outputPaths[0] && (
          <>
            <div>
              <dt>出力パス</dt>
              <dd className="selectable">{displayPath(outputPaths[0])}</dd>
            </div>
            <div>
              <dt>ファイル名</dt>
              <dd className="selectable">{fileName(displayPath(outputPaths[0]))}</dd>
            </div>
            <div>
              <dt>保存先</dt>
              <dd className="selectable">{directoryName(displayPath(outputPaths[0]))}</dd>
            </div>
          </>
        )}
        {completion.kind === "resourcePack" && (
          <div>
            <dt>配置</dt>
            <dd>{runDetails?.directInstall ? "インスタンスに直接配置" : "選択した保存先"}</dd>
          </div>
        )}
      </dl>
      {outputPaths.length > 1 && (
        <section className="export-completion__section" aria-label="出力パス">
          <h2>出力パス</h2>
          <ul>
            {outputPaths.map((path) => <li className="selectable" key={path}>{displayPath(path)}</li>)}
          </ul>
        </section>
      )}
      {!!runDetails?.modResults.length && (
        <section className="export-completion__section" aria-label="対象ごとの結果">
          <h2>対象ごとの結果</h2>
          <ul className="export-completion__mod-results">
            {runDetails.modResults.map((result) => (
              <li key={result.modUnitId}>
                <strong>{result.modName}</strong>
                <span>{statusLabel(result.status)}</span>
                {result.outputPath && <code className="selectable">{displayPath(result.outputPath)}</code>}
                {result.message && <p>{result.message}</p>}
              </li>
            ))}
          </ul>
        </section>
      )}
      {!!runDetails?.conflicts.length && (
        <details className="export-completion__section">
          <summary>{runDetails.conflicts.length.toLocaleString("ja-JP")} 件のキー衝突</summary>
          <ul className="export-completion__conflicts">
            {runDetails.conflicts.map((conflict, index) => (
              <li key={`${conflict.namespace}:${conflict.key}:${index}`}>
                <code>{conflict.namespace} / {conflict.key}</code>
                <span>採用: {conflict.keptFrom}</span>
                <span>破棄: {conflict.droppedFrom}</span>
              </li>
            ))}
          </ul>
        </details>
      )}
      <div className="export-completion__counts" aria-label="翻訳件数">
        <p>{completion.translatedCount.toLocaleString("ja-JP")} 件 翻訳済み</p>
        {completion.missingCount > 0 && (
          <p className="export-completion__missing">未翻訳 合計 {completion.missingCount.toLocaleString("ja-JP")} 件</p>
        )}
      </div>
      <p className="export-completion__instruction">{instruction}</p>
      {error && <p className="status-message" role="alert">{error}</p>}
      <footer className="export-completion__actions">
        {outputPaths.length > 0 && <PrimaryButton onClick={() => void showOutputFolder()}>フォルダを開く</PrimaryButton>}
        <SecondaryButton onClick={() => void goTo("editor")}>翻訳に戻る</SecondaryButton>
        <SecondaryButton onClick={() => void goTo("export")}>別の形式で書き出す</SecondaryButton>
      </footer>
    </section>
  );
}
