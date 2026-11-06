import { useEffect, useRef, useState } from "react";

import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { formatNumber, useT } from "../../i18n";
import { targetLanguageName } from "../../i18n/minecraftLanguages";
import { AppCommandError, openOutputFolder } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import type { ExportKind } from "../../types/export";
import { getExportRunDetails } from "./completionData";
import { displayPath } from "./pathDisplay";
import "./ExportCompletionScreen.css";

const FORMAT_LABELS = {
  resourcePack: "export.format.resourcePack",
  modifiedJar: "export.completion.format.modifiedJar",
  languageFile: "export.format.languageFile",
  modpackFiles: "export.format.modpackFiles",
  shaderpackZip: "export.format.shaderpackZip",
} as const;

function formatLabel(kind: ExportKind, translate: ReturnType<typeof useT>, language: string) {
  if (kind === "shaderpackZip") return translate(FORMAT_LABELS.shaderpackZip, { language });
  return translate(FORMAT_LABELS[kind]);
}

function errorMessage(error: unknown, translate: ReturnType<typeof useT>) {
  void logFrontendError(error, "export completion screen");
  return error instanceof AppCommandError ? error.message : translate("export.error.openFolder");
}

function fileName(path: string) {
  return path.split(/[\\/]/).at(-1) || "—";
}

function directoryName(path: string) {
  const separator = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return separator >= 0 ? path.slice(0, separator) : "—";
}

function statusLabel(status: string, translate: ReturnType<typeof useT>) {
  switch (status) {
    case "success": return translate("export.completion.status.success");
    case "skipped": return translate("export.completion.status.skipped");
    default: return translate("export.completion.status.failed");
  }
}

export function ExportCompletionScreen() {
  const t = useT();
  const completion = useAppStore((state) => state.exportCompletion);
  const goTo = useAppStore((state) => state.goTo);
  const targetLocale = useProjectStore((state) => state.details?.targetLocale ?? "ja_jp");
  const language = targetLanguageName(targetLocale);
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
      setError(errorMessage(cause, t));
    }
  };

  useEffect(() => {
    if (!completion?.autoOpenOutputFolder || autoOpenStarted.current || !firstOutput) return;
    autoOpenStarted.current = true;
    setError(null);
    void openOutputFolder(firstOutput).catch((cause: unknown) => setError(errorMessage(cause, t)));
  }, [completion?.autoOpenOutputFolder, firstOutput, t]);

  if (!completion) {
    return <section className="focus-page export-completion" aria-live="polite">{t("export.error.noCompletion")}</section>;
  }

  let instruction: string;
  switch (completion.kind) {
    case "resourcePack":
      instruction = t("export.completion.instruction.resourcePack");
      break;
    case "modifiedJar":
      instruction = runDetails?.inPlaceReplacement
        ? t("export.completion.instruction.modifiedJarRestored")
        : t("export.completion.instruction.modifiedJarUnchanged");
      break;
    case "languageFile":
      instruction = completion.sourceKind === "shaderpack"
        ? t("export.completion.instruction.shaderpackLanguageDestination")
        : t("export.completion.instruction.languageDestination", { namespace: completion.namespace });
      break;
    case "modpackFiles":
      instruction = runDetails?.directInstall
        ? t("export.completion.instruction.modpackDirect")
        : t("export.completion.instruction.modpackZip");
      break;
    case "shaderpackZip":
      instruction = runDetails?.directInstall
        ? t("export.completion.instruction.shaderpackDirectInstall", { language })
        : t("export.completion.instruction.shaderpackSelect", { language });
      break;
  }
  if (runDetails?.shaderpackInstalled) {
    instruction = t("export.completion.instruction.withShaderpackInstall", {
      instruction,
      shaderpackInstruction: t("export.completion.instruction.shaderpackDirectInstall", { language }),
    });
  }

  const successfulCount = runDetails?.modResults.filter((result) => result.status === "success").length ?? 0;
  const hasFailedMod = runDetails?.modResults.some((result) => result.status !== "success") ?? false;
  const allFailed = hasFailedMod && successfulCount === 0 && outputPaths.length === 0;

  return (
    <section className="focus-page export-completion" aria-labelledby="export-completion-title">
      <h1 className="page-title" id="export-completion-title">
        {t(allFailed ? "export.completion.title.failed" : "export.completion.title.success")}
      </h1>
      <dl className="export-completion__details">
        <div>
          <dt>{t("export.completion.field.format")}</dt>
          <dd>{formatLabel(completion.kind, t, language)}</dd>
        </div>
        {outputPaths[0] && (
          <>
            <div>
              <dt>{t("export.completion.field.outputPath")}</dt>
              <dd className="selectable">{displayPath(outputPaths[0])}</dd>
            </div>
            <div>
              <dt>{t("export.completion.field.fileName")}</dt>
              <dd className="selectable">{fileName(displayPath(outputPaths[0]))}</dd>
            </div>
            <div>
              <dt>{t("export.completion.field.saveLocation")}</dt>
              <dd className="selectable">{directoryName(displayPath(outputPaths[0]))}</dd>
            </div>
          </>
        )}
        {completion.kind === "resourcePack" && (
          <div>
            <dt>{t("export.completion.field.placement")}</dt>
            <dd>{t(runDetails?.directInstall
              ? "export.completion.destination.instance"
              : "export.completion.destination.selected")}</dd>
          </div>
        )}
      </dl>
      {outputPaths.length > 1 && (
        <section className="export-completion__section" aria-label={t("export.completion.section.outputPaths")}>
          <h2>{t("export.completion.section.outputPaths")}</h2>
          <ul>
            {outputPaths.map((path) => <li className="selectable" key={path}>{displayPath(path)}</li>)}
          </ul>
        </section>
      )}
      {!!runDetails?.modResults.length && (
        <section className="export-completion__section" aria-label={t("export.completion.section.unitResults")}>
          <h2>{t("export.completion.section.unitResults")}</h2>
          <ul className="export-completion__mod-results">
            {runDetails.modResults.map((result) => (
              <li key={result.modUnitId}>
                <strong>{result.modName}</strong>
                <span>{statusLabel(result.status, t)}</span>
                {result.outputPath && <code className="selectable">{displayPath(result.outputPath)}</code>}
                {result.message && <p>{result.message}</p>}
              </li>
            ))}
          </ul>
        </section>
      )}
      {!!runDetails?.conflicts.length && (
        <details className="export-completion__section">
          <summary>{t(runDetails.conflicts.length === 1
            ? "export.completion.conflictSummary_one"
            : "export.completion.conflictSummary", { count: formatNumber(runDetails.conflicts.length) })}</summary>
          <ul className="export-completion__conflicts">
            {runDetails.conflicts.map((conflict, index) => (
              <li key={`${conflict.namespace}:${conflict.key}:${index}`}>
                <code>{conflict.namespace} / {conflict.key}</code>
                <span>{t("export.completion.conflictKept", { source: conflict.keptFrom })}</span>
                <span>{t("export.completion.conflictDropped", { source: conflict.droppedFrom })}</span>
              </li>
            ))}
          </ul>
        </details>
      )}
      <div className="export-completion__counts" aria-label={t("export.completion.translationCounts")}>
        <p>{t(completion.translatedCount === 1
          ? "export.completion.translatedCount_one"
          : "export.completion.translatedCount", { count: formatNumber(completion.translatedCount) })}</p>
        {completion.missingCount > 0 && (
          <p className="export-completion__missing">{t(completion.missingCount === 1
            ? "export.completion.missingCount_one"
            : "export.completion.missingCount", { count: formatNumber(completion.missingCount) })}</p>
        )}
      </div>
      {!!runDetails?.placeholderErrorCount && (
        <p className="export-completion__missing">{t(runDetails.placeholderErrorCount === 1
          ? "export.completion.placeholderErrorCount_one"
          : "export.completion.placeholderErrorCount", { count: formatNumber(runDetails.placeholderErrorCount) })}</p>
      )}
      <p className="export-completion__instruction">{instruction}</p>
      {runDetails?.shaderpackError && (
        <p className="status-message" role="alert">{t("export.completion.shaderpackError", { error: runDetails.shaderpackError })}</p>
      )}
      {error && <p className="status-message" role="alert">{error}</p>}
      <footer className="export-completion__actions">
        {outputPaths.length > 0 && <PrimaryButton onClick={() => void showOutputFolder()}>{t("export.completion.action.openFolder")}</PrimaryButton>}
        <SecondaryButton onClick={() => void goTo("editor")}>{t("export.completion.action.backToTranslation")}</SecondaryButton>
        <SecondaryButton onClick={() => void goTo("export")}>{t("export.completion.action.exportAnother")}</SecondaryButton>
      </footer>
    </section>
  );
}
