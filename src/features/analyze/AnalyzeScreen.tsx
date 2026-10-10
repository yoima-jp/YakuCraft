import { useState } from "react";

import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { Disclosure } from "../../components/Controls";
import { ProgressBar } from "../../components/progress/ProgressBar";
import { AppCommandError, cancelAnalysis } from "../../lib/tauri";
import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import { useScanStore } from "../scan/scanStore";
import "./AnalyzeScreen.css";

const STAGES = [
  "validateJar",
  "sha256",
  "scanZipPaths",
  "parseMetadata",
  "scanNestedJars",
  "discoverNamespaces",
  "parseLanguages",
  "detectTargetTranslation",
];

export function AnalyzeScreen() {
  const [cancelRequested, setCancelRequested] = useState(false);
  const fileName = useProjectStore((state) => state.sourceFileName);
  const progress = useProjectStore((state) => state.analysisProgress);
  const jobId = useProjectStore((state) => state.analysisJobId);
  const analysisError = useProjectStore((state) => state.analysisError);
  const modImportFailures = useProjectStore((state) => state.modImportFailures);
  const setAnalysisError = useProjectStore((state) => state.setAnalysisError);
  const setModImportFailures = useProjectStore((state) => state.setModImportFailures);
  const scanCreationComplete = useScanStore((state) => state.creationComplete);
  const goTo = useAppStore((state) => state.goTo);

  const stageIndex = progress ? Math.max(0, STAGES.indexOf(progress.stage)) : 0;
  const stageFraction = progress?.total && progress.total > 0
    ? Math.min(1, progress.current / progress.total)
    : 0;
  const progressValue = progress
    ? Math.round(((stageIndex + stageFraction) / STAGES.length) * 100)
    : 0;

  const cancel = async () => {
    if (!jobId) return;
    setCancelRequested(true);
    try {
      await cancelAnalysis(jobId);
    } catch (error: unknown) {
      const message = error instanceof AppCommandError ? error.message : "処理を中止できませんでした。";
      setCancelRequested(false);
      setAnalysisError(error instanceof AppCommandError
        ? error.appError
        : { code: "unexpected", userMessage: message, recoverable: true });
    }
  };

  return (
    <section className="focus-page analyze-screen" aria-labelledby="analyze-title">
      <div className="analyze-screen__content">
        <p className="eyebrow">{fileName}</p>
        <h1 className="page-title" id="analyze-title">
          {analysisError
            ? "解析できませんでした"
            : scanCreationComplete && modImportFailures.length > 0
              ? "読み込めなかったModがあります"
              : "Modを調べています"}
        </h1>
        {!analysisError && !scanCreationComplete && (
          <>
            <ProgressBar value={progressValue} label="解析の進行状況" />
            <p className="analyze-screen__stage" aria-live="polite">
              {progress?.modFilename && progress.modIndex && progress.modCount
                ? `${progress.modIndex} / ${progress.modCount} · ${progress.modFilename} · ${progress.message}`
                : progress?.message ?? "準備中…"}
            </p>
            {jobId && (
              <SecondaryButton disabled={cancelRequested} onClick={() => void cancel()}>
                {cancelRequested ? "中止中…" : "キャンセル"}
              </SecondaryButton>
            )}
          </>
        )}
        {analysisError && (
          <div className="analyze-screen__error" role="alert">
            <p>{analysisError.userMessage}</p>
            {analysisError.detail && (
              <Disclosure title="詳細">
                <pre>{analysisError.detail}</pre>
              </Disclosure>
            )}
          </div>
        )}
        {!analysisError && scanCreationComplete && modImportFailures.length > 0 && (
          <section className="analyze-screen__failures" aria-label="読み込めなかったMod" aria-live="polite">
            <p>{modImportFailures.length.toLocaleString("ja-JP")} 個のModを読み込めませんでした</p>
            <ul>
              {modImportFailures.map((failure) => (
                <li key={failure.filename}>
                  <span>{failure.filename}</span>
                  <span className="selectable">{failure.reason}</span>
                </li>
              ))}
            </ul>
            <PrimaryButton onClick={() => {
              setModImportFailures([]);
              void goTo("review");
            }}>確認へ進む</PrimaryButton>
          </section>
        )}
      </div>
      {analysisError && (
        <PrimaryButton onClick={() => void goTo("home")}>ホームへ戻る</PrimaryButton>
      )}
    </section>
  );
}
