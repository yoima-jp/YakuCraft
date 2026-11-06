import { useState } from "react";

import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { Disclosure } from "../../components/Controls";
import { ProgressBar } from "../../components/progress/ProgressBar";
import { AppCommandError, cancelAnalysis } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import { useScanStore } from "../scan/scanStore";
import { formatNumber, useT } from "../../i18n";
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
  const translate = useT();
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
      void logFrontendError(error, "cancel analysis");
      const message = error instanceof AppCommandError ? error.message : translate("analyze.cancelError");
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
            ? translate("analyze.failedTitle")
            : scanCreationComplete && modImportFailures.length > 0
              ? translate("analyze.failuresTitle")
              : translate("analyze.title")}
        </h1>
        {!analysisError && !scanCreationComplete && (
          <>
            <ProgressBar value={progressValue} label={translate("analyze.progress")} />
            <p className="analyze-screen__stage" aria-live="polite">
              {progress?.modFilename && progress.modIndex && progress.modCount
                ? `${formatNumber(progress.modIndex)} / ${formatNumber(progress.modCount)} · ${progress.modFilename} · ${progress.message}`
                : progress?.message ?? translate("common.preparing")}
            </p>
            {jobId && (
              <SecondaryButton disabled={cancelRequested} onClick={() => void cancel()}>
                {cancelRequested ? translate("analyze.canceling") : translate("analyze.cancel")}
              </SecondaryButton>
            )}
          </>
        )}
        {analysisError && (
          <div className="analyze-screen__error" role="alert">
            <p>{analysisError.userMessage}</p>
            {analysisError.detail && (
              <Disclosure title={translate("analyze.details")}>
                <pre>{analysisError.detail}</pre>
              </Disclosure>
            )}
          </div>
        )}
        {!analysisError && scanCreationComplete && modImportFailures.length > 0 && (
          <section className="analyze-screen__failures" aria-label={translate("analyze.failuresTitle")} aria-live="polite">
            <p>{modImportFailures.length === 1
              ? translate("analyze.failureCount_one", { count: formatNumber(modImportFailures.length) })
              : translate("analyze.failureCount", { count: formatNumber(modImportFailures.length) })}</p>
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
            }}>{translate("analyze.proceedReview")}</PrimaryButton>
          </section>
        )}
      </div>
      {analysisError && (
        <PrimaryButton onClick={() => void goTo("home")}>{translate("analyze.backHome")}</PrimaryButton>
      )}
    </section>
  );
}
