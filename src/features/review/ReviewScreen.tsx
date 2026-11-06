import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import { PrimaryButton, SecondaryButton, TertiaryButton } from "../../components/button/Buttons";
import { Checkbox, Disclosure, RadioGroup, Select } from "../../components/Controls";
import { StepProgress } from "../../components/progress/StepProgress";
import { localeDisplayName } from "../../lib/locale";
import { compareText, formatNumber, t, useT } from "../../i18n";
import { minecraftLanguageLabel, targetLanguageName } from "../../i18n/minecraftLanguages";
import { AppCommandError, getReviewAssetSummary, listEntries, saveReviewSettings } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { useAppStore } from "../../stores/appStore";
import { useEditorStore } from "../../stores/editorStore";
import { useProjectStore } from "../../stores/projectStore";
import type { OperationProgressDto } from "../../types/commands";
import type {
  IncludedNamespaceDto,
  MinecraftVersionDetectionDto,
  ProjectAssetSummaryDto,
  ReviewSetupSummaryDto,
} from "../../types/project";
import "./ReviewScreen.css";

type ReviewStep = "mod" | "sourceLanguage" | "existingTarget" | "missingKeys" | "assets";

const EMPTY_ASSET_SUMMARY: ProjectAssetSummaryDto = {
  total: 0,
  blockModels: 0,
  itemModels: 0,
  entityTextures: 0,
  geometryModels: 0,
  unresolvedEntries: 0,
};

function selectionKey(sourceLocale: string, namespaces: IncludedNamespaceDto[]) {
  const selections = namespaces
    .map(({ modUnitId, namespace }) => [modUnitId, namespace] as const)
    .sort(([leftId, leftNamespace], [rightId, rightNamespace]) => leftId - rightId || compareText(leftNamespace, rightNamespace));
  return JSON.stringify([sourceLocale.toLowerCase(), selections]);
}

function isNamespaceIncluded(selections: IncludedNamespaceDto[], modUnitId: number, namespace: string) {
  return selections.some((selection) => selection.modUnitId === modUnitId && selection.namespace === namespace);
}

function minecraftVersionEvidence(detection: MinecraftVersionDetectionDto | null) {
  if (!detection) return null;
  if (detection.method === "metadata") {
    return detection.rounded
      ? t("review.modVersionNear", { version: detection.originalValue ?? "" })
      : t("review.modVersionFrom", { version: detection.originalValue ?? "" });
  }
  if (detection.method === "packMcmeta") {
    return detection.rounded
      ? t("review.packVersionFromNear", { version: detection.originalValue ?? t("review.unknownPackValue") })
      : t("review.packVersionFrom", { version: detection.originalValue ?? t("review.unknownPackValue") });
  }
  return detection.rounded
    ? t("review.resourceVersionInferredNear")
    : t("review.resourceVersionInferred");
}

export function ReviewScreen() {
  const translate = useT();
  const details = useProjectStore((state) => state.details);
  const [step, setStep] = useState<ReviewStep>("mod");
  const [showSourceLanguageOptions, setShowSourceLanguageOptions] = useState(false);
  const [saving, setSaving] = useState(false);
  const [reviewSummaryRetry, setReviewSummaryRetry] = useState(0);
  const [reviewSummaryProgress, setReviewSummaryProgress] = useState<{
    key: string;
    progress: OperationProgressDto;
  } | null>(null);
  const [reviewSummaryState, setReviewSummaryState] = useState<{
    key: string;
    summary: ReviewSetupSummaryDto;
  } | null>(null);
  const modListRef = useRef<HTMLUListElement>(null);
  const sourceLocale = useProjectStore((state) => state.sourceLocale);
  const setSourceLocale = useProjectStore((state) => state.setSourceLocale);
  const preserveTarget = useProjectStore((state) => state.preserveTarget);
  const setPreserveTarget = useProjectStore((state) => state.setPreserveTarget);
  const fillMissingSourceKeys = useProjectStore((state) => state.fillMissingSourceKeys);
  const setFillMissingSourceKeys = useProjectStore((state) => state.setFillMissingSourceKeys);
  const includedNamespaces = useProjectStore((state) => state.includedNamespaces);
  const modUnits = useProjectStore((state) => state.modUnits);
  const modImportFailures = useProjectStore((state) => state.modImportFailures);
  const setIncludedNamespaces = useProjectStore((state) => state.setIncludedNamespaces);
  const reviewError = useProjectStore((state) => state.reviewError);
  const setReviewError = useProjectStore((state) => state.setReviewError);
  const setProjectDetails = useProjectStore((state) => state.setProjectDetails);
  const setOperationProgress = useProjectStore((state) => state.setOperationProgress);
  const setOperationTitle = useProjectStore((state) => state.setOperationTitle);
  const goTo = useAppStore((state) => state.goTo);

  // entryCountにはPatchouli文書Entryも含まれ、LanguageSummaryのない名前空間がある。
  const nestedNamespaces = useMemo(() => details?.namespaces.filter((namespace) =>
    namespace.containerPath && namespace.entryCount > 0,
  ) ?? [], [details]);
  const nestedNamespaceGroups = useMemo(() => {
    const archives = new Map((details?.nestedArchives ?? []).map((archive) => [
      `${archive.modUnitId}:${archive.archivePath}`,
      archive,
    ]));
    const groups = new Map<string, {
      key: string;
      name: string;
      namespaces: typeof nestedNamespaces;
    }>();
    for (const namespace of nestedNamespaces) {
      const key = `${namespace.modUnitId}:${namespace.containerPath}`;
      let group = groups.get(key);
      if (!group) {
        const archive = archives.get(key);
        const parent = modUnits.find((unit) => unit.id === namespace.modUnitId);
        const childName = archive?.name
          ?? archive?.modId
          ?? namespace.containerPath.split(/[\\/]/).at(-1)
          ?? namespace.namespace;
        group = {
          key,
          name: `${parent?.name ?? parent?.modId ?? `Mod ${namespace.modUnitId}`} / ${childName}`,
          namespaces: [],
        };
        groups.set(key, group);
      }
      group.namespaces.push(namespace);
    }
    return [...groups.values()];
  }, [details, modUnits, nestedNamespaces]);
  const namespacesByModUnit = useMemo(() => {
    const namespaces = new Map<number, Set<string>>();
    for (const { modUnitId, namespace } of details?.namespaces ?? []) {
      const values = namespaces.get(modUnitId) ?? new Set<string>();
      values.add(namespace);
      namespaces.set(modUnitId, values);
    }
    return new Map([...namespaces].map(([modUnitId, values]) => [modUnitId, [...values].sort()]));
  }, [details]);
  // 名前空間チップは折り返すため、表示した行の実寸を計測して後続行の位置を保つ。
  // 多数のModでは可視範囲に応じて行を差し替えるため、仮想化器の戻り値をReact Compilerでmemo化しない。
  // oxlint-disable-next-line react/incompatible-library
  const modListVirtualizer = useVirtualizer({
    count: modUnits.length,
    getScrollElement: () => modListRef.current,
    estimateSize: () => 82,
    getItemKey: (index) => modUnits[index]?.id ?? index,
    overscan: 6,
  });
  const nestedModCount = nestedNamespaceGroups.length;
  const sourceLocales = useMemo(() => {
    if (!details) return [];
    const counts = new Map<string, number>();
    for (const language of details.languages.filter((item) =>
      isNamespaceIncluded(includedNamespaces, item.modUnitId, item.namespace))) {
      counts.set(language.locale, (counts.get(language.locale) ?? 0) + language.entryCount);
    }
    return [...counts].sort(([left], [right]) => compareText(left, right));
  }, [details, includedNamespaces]);
  const currentSelectionKey = selectionKey(sourceLocale, includedNamespaces);
  const persistedSelectionKey = details
    ? selectionKey(details.sourceLocale, details.includedNamespaces)
    : "";
  const persistedSelectionsMatch = details !== null && persistedSelectionKey === currentSelectionKey;
  // Step 1の選択変更後は集計が完了するまで「次へ」を止める。以前は各キーで全Assetを走査していたため、Createではこの待機が停止に見えていた。
  const reviewSummaryReady = persistedSelectionsMatch || reviewSummaryState?.key === currentSelectionKey;
  const reviewSummaryLoading = !reviewSummaryReady && reviewError === null;
  const currentReviewSummary = useMemo<ReviewSetupSummaryDto | null>(() => {
    if (!details) return null;
    const loadedSummary = reviewSummaryState?.key === currentSelectionKey ? reviewSummaryState.summary : null;
    if (persistedSelectionsMatch) {
      return {
        existingTargetTranslationCount: details.existingTargetTranslationCount,
        sameLanguageEntryCount: loadedSummary?.sameLanguageEntryCount ?? 0,
        missingSourceKeysCount: details.missingSourceKeysCount,
        assets: details.assetSummary ?? EMPTY_ASSET_SUMMARY,
      };
    }
    return loadedSummary;
  }, [currentSelectionKey, details, persistedSelectionsMatch, reviewSummaryState]);
  const currentReviewSummaryLoaded = reviewSummaryState?.key === currentSelectionKey;
  const displayedReviewSummary = useMemo(() => currentReviewSummary ?? (details ? {
    existingTargetTranslationCount: details.existingTargetTranslationCount,
    sameLanguageEntryCount: 0,
    missingSourceKeysCount: details.missingSourceKeysCount,
    assets: details.assetSummary ?? EMPTY_ASSET_SUMMARY,
  } : null), [currentReviewSummary, details]);
  const visibleSteps = useMemo<ReviewStep[]>(() => {
    const steps: ReviewStep[] = ["mod", "sourceLanguage"];
    if ((displayedReviewSummary?.existingTargetTranslationCount ?? 0) > 0) {
      steps.push("existingTarget");
    }
    if ((displayedReviewSummary?.missingSourceKeysCount ?? 0) > 0) {
      steps.push("missingKeys");
    }
    if ((displayedReviewSummary?.assets.total ?? details?.assetCount ?? 0) > 0) {
      steps.push("assets");
    }
    return steps;
  }, [details?.assetCount, displayedReviewSummary]);
  const selectedLocaleCount = details?.languages
    .filter((language) => isNamespaceIncluded(includedNamespaces, language.modUnitId, language.namespace)
      && language.locale.toLowerCase() === sourceLocale.toLowerCase())
    .reduce((total, language) => total + language.entryCount, 0) ?? 0;

  const toggleNamespace = (modUnitId: number, namespace: string) => {
    setIncludedNamespaces(isNamespaceIncluded(includedNamespaces, modUnitId, namespace)
      ? includedNamespaces.filter((value) => value.modUnitId !== modUnitId || value.namespace !== namespace)
      : [...includedNamespaces, { modUnitId, namespace }]);
  };

  const setAllNestedNamespaces = (included: boolean) => {
    const nestedKeys = new Set(nestedNamespaces.map(({ modUnitId, namespace }) => `${modUnitId}:${namespace}`));
    if (!included) {
      setIncludedNamespaces(includedNamespaces.filter((selection) => !nestedKeys.has(`${selection.modUnitId}:${selection.namespace}`)));
      return;
    }
    const selectedKeys = new Set(includedNamespaces.map(({ modUnitId, namespace }) => `${modUnitId}:${namespace}`));
    setIncludedNamespaces([
      ...includedNamespaces,
      ...nestedNamespaces
        .filter(({ modUnitId, namespace }) => !selectedKeys.has(`${modUnitId}:${namespace}`))
        .map(({ modUnitId, namespace }) => ({ modUnitId, namespace })),
    ]);
  };

  useEffect(() => {
    if (!sourceLocales.length || sourceLocales.some(([locale]) => locale.toLowerCase() === sourceLocale.toLowerCase())) {
      return;
    }
    const recommendedLocale = details?.recommendedSourceLocale?.toLowerCase();
    const nonTargetLocales = sourceLocales.filter(
      ([locale]) => locale.toLowerCase() !== details?.targetLocale.toLowerCase(),
    );
    const fallback = nonTargetLocales.find(([locale]) => locale.toLowerCase() === recommendedLocale)
      ?? nonTargetLocales[0]
      ?? sourceLocales[0];
    setSourceLocale(fallback[0]);
  }, [details?.recommendedSourceLocale, details?.targetLocale, setSourceLocale, sourceLocale, sourceLocales]);

  useEffect(() => {
    if (!details || reviewSummaryState?.key === currentSelectionKey) return;
    let active = true;
    setReviewError(null);
    void getReviewAssetSummary({
      sourceLocale,
      fallbackPriority: details.fallbackPriority,
      preserveExistingTarget: details.preserveExistingTarget,
      fillMissingFromOtherLanguages: details.fillMissingFromOtherLanguages,
      includedNamespaces: [...includedNamespaces],
    }, (progress) => {
      if (active) setReviewSummaryProgress({ key: currentSelectionKey, progress });
    }).then((summary) => {
      if (active) setReviewSummaryState({ key: currentSelectionKey, summary });
    }).catch((error: unknown) => {
      if (!active) return;
      void logFrontendError(error, "review summary");
      setReviewError(error instanceof AppCommandError
        ? error.appError
        : { code: "unexpected", userMessage: t("review.loadItemsError"), recoverable: true });
    });
    return () => {
      active = false;
    };
  }, [
    currentSelectionKey,
    details,
    includedNamespaces,
    reviewSummaryState,
    reviewSummaryRetry,
    setReviewError,
    sourceLocale,
  ]);

  const goBack = () => {
    const index = visibleSteps.indexOf(step);
    if (index <= 0) {
      void goTo("home");
      return;
    }
    setStep(visibleSteps[index - 1]);
  };

  const goNext = async () => {
    if (!details) return;
    if (!reviewSummaryReady) {
      setReviewSummaryRetry((attempt) => attempt + 1);
      return;
    }
    const index = visibleSteps.indexOf(step);
    if (index < visibleSteps.length - 1) {
      setStep(visibleSteps[index + 1]);
      return;
    }
    setSaving(true);
    setReviewError(null);
    setOperationTitle(translate("review.preparingTranslation"));
    setOperationProgress({ percent: 0, message: translate("review.savingSettings") });
    try {
      await goTo("loading");
      const missingSourceKeysCount = currentReviewSummary?.missingSourceKeysCount ?? 0;
      const savedDetails = await saveReviewSettings({
        sourceLocale,
        fallbackPriority: details.fallbackPriority,
        // 0件で画面を省いても、Preserveの既定選択を設定として維持する。
        preserveExistingTarget: preserveTarget,
        fillMissingFromOtherLanguages: missingSourceKeysCount === 0 || fillMissingSourceKeys,
        includedNamespaces: [...includedNamespaces],
      }, setOperationProgress);
      setProjectDetails(savedDetails);
      setOperationProgress({ percent: null, message: translate("common.loadingTranslations") });
      const entries = await listEntries(modUnits[0]?.id ?? null);
      useEditorStore.getState().loadEntries(entries, savedDetails.project.id);
      await goTo("editor");
    } catch (error: unknown) {
      void logFrontendError(error, "save review settings");
      setOperationProgress(null);
      setReviewError(error instanceof AppCommandError
        ? error.appError
        : { code: "unexpected", userMessage: t("review.savingError"), recoverable: true });
      await goTo("review");
    } finally {
      setSaving(false);
      setOperationProgress(null);
    }
  };

  if (!details) {
    return <section className="focus-page review-screen"><output aria-live="polite">{translate("review.loadingProject")}</output></section>;
  }

  const currentStepIndex = visibleSteps.indexOf(step);
  const assetSummary = currentReviewSummary?.assets ?? details.assetSummary ?? EMPTY_ASSET_SUMMARY;
  const existingTargetCount = currentReviewSummary?.existingTargetTranslationCount ?? 0;
  const missingSourceKeyCount = currentReviewSummary?.missingSourceKeysCount ?? 0;
  const sameLanguageEntryCount = currentReviewSummary?.sameLanguageEntryCount ?? 0;
  let content;
  switch (step) {
    case "mod":
      content = (
        <>
          <h1 className="page-title">{translate("review.modInfoTitle")}</h1>
          <dl className={`review-details${modUnits.length > 1 ? " review-details--mods" : ""}`}>
            {modUnits.length > 1 ? (
              <div>
                <dt>{translate("review.totalTargets", { mods: formatNumber(modUnits.length), namespaces: formatNumber(details.namespaces.length) })}</dt>
                <dd>
                  <ul
                    className="review-mod-list"
                    ref={modListRef}
                    aria-label={translate("review.modsInProject")}
                    style={{ height: Math.min(348, Math.max(82, modListVirtualizer.getTotalSize())) }}
                  >
                    <li className="review-mod-list__space" aria-hidden="true" style={{ height: modListVirtualizer.getTotalSize() }} />
                    {modListVirtualizer.getVirtualItems().map((virtualRow) => {
                      const unit = modUnits[virtualRow.index];
                      if (!unit) return null;
                      return (
                        <li
                          className="review-mod-list__item"
                          key={unit.id}
                          ref={modListVirtualizer.measureElement}
                          data-index={virtualRow.index}
                          style={{ transform: `translateY(${virtualRow.start}px)` }}
                        >
                          <strong>{unit.sourceKind === "modpack"
                            ? translate("review.modpackUnit")
                            : unit.sourceKind === "shaderpack"
                              ? translate("review.shaderpackFile", { filename: unit.sourceFilename })
                              : unit.name ?? unit.modId ?? translate("review.unknown")}</strong>
                          {(unit.sourceKind === undefined || unit.sourceKind === "jar") && (
                            <span className="review-mod-list__version">{unit.version ?? translate("review.unknownVersion")} · {unit.sourceFilename}</span>
                          )}
                          {unit.sourceKind !== "shaderpack" && <div className="review-mod-list__namespaces">
                            {(namespacesByModUnit.get(unit.id) ?? []).map((namespace) => (
                              <span className="review-mod-list__namespace" key={namespace}>{namespace}</span>
                            ))}
                          </div>}
                        </li>
                      );
                    })}
                  </ul>
                </dd>
              </div>
            ) : (
              <>
                <div>
                  <dt>{translate("review.file")}</dt>
                  <dd>{modUnits[0]?.sourceKind === "modpack"
                    ? translate("review.modpackUnit")
                    : modUnits[0]?.sourceFilename ?? details.project.displayName}</dd>
                </div>
                {modUnits[0]?.sourceKind !== "shaderpack" && (
                  <>
                    <div><dt>{translate("review.modName")}</dt><dd>{details.mods[0]?.name ?? translate("review.unknown")}</dd></div>
                    <div><dt>{translate("review.version")}</dt><dd>{details.mods[0]?.version ?? translate("review.unknown")}</dd></div>
                    <div>
                      <dt>{translate("review.minecraft")}</dt>
                      <dd>
                        {details.minecraftVersionDetection
                          ? details.minecraftVersionDetection.version
                          : details.mods[0]?.minecraftVersion ?? translate("review.unknown")}
                        {details.minecraftVersionDetection && (
                          <small className="review-details__hint">{minecraftVersionEvidence(details.minecraftVersionDetection)}</small>
                        )}
                      </dd>
                    </div>
                    <div><dt>{translate("review.loader")}</dt><dd>{details.mods[0]?.loader ?? translate("review.unknown")}</dd></div>
                  </>
                )}
                <div>
                  <dt>{translate("review.namespaces")}</dt>
                  <dd>{details.namespaces.map((item) => item.namespace).join(translate("review.namespaceSeparator")) || translate("review.unknown")}</dd>
                </div>
              </>
            )}
          </dl>
          {modImportFailures.length > 0 && (
            <section aria-label={translate("review.failedJars")} aria-live="polite">
              <p>{translate("review.failedJars")}</p>
              <ul>
                {modImportFailures.map((failure) => (
                  <li key={failure.filename}>{failure.filename} · {failure.reason}</li>
                ))}
              </ul>
            </section>
          )}
          {nestedModCount > 0 && nestedNamespaces.length > 0 && (
            <>
              <p className="review-note">{nestedModCount === 1
                ? translate("review.nestedModCount_one", { count: formatNumber(nestedModCount) })
                : translate("review.nestedModCount", { count: formatNumber(nestedModCount) })}</p>
              <Disclosure className="review-nested" title={translate("review.includeTargets")}>
                {modUnits.length > 1 ? (
                  <>
                    <div className="review-nested__bulk-actions">
                      <TertiaryButton onClick={() => setAllNestedNamespaces(true)}>{translate("review.selectAll")}</TertiaryButton>
                      <TertiaryButton onClick={() => setAllNestedNamespaces(false)}>{translate("review.clearAll")}</TertiaryButton>
                    </div>
                    <div className="review-nested__groups">
                      {nestedNamespaceGroups.map((group) => (
                        <Disclosure
                          className="review-nested__group"
                          key={group.key}
                          title={translate("review.namespaceGroup", { name: group.name, count: formatNumber(group.namespaces.length) })}
                        >
                          <fieldset className="review-options">
                            <legend className="visually-hidden">{translate("review.nestedNamespacesFor", { name: group.name })}</legend>
                            {group.namespaces.map((namespace) => (
                              <Checkbox
                                className="review-option"
                                key={`${namespace.modUnitId}:${namespace.namespace}`}
                                name="nested-namespace"
                                checked={isNamespaceIncluded(includedNamespaces, namespace.modUnitId, namespace.namespace)}
                                onChange={() => toggleNamespace(namespace.modUnitId, namespace.namespace)}
                              >
                                {namespace.namespace} · {formatNumber(namespace.entryCount)}
                              </Checkbox>
                            ))}
                          </fieldset>
                        </Disclosure>
                      ))}
                    </div>
                  </>
                ) : (
                  <fieldset className="review-options">
                    <legend className="visually-hidden">{translate("review.nestedNamespaces")}</legend>
                    {nestedNamespaces.map((namespace) => (
                      <Checkbox
                        className="review-option"
                        key={`${namespace.modUnitId}:${namespace.namespace}`}
                        name="nested-namespace"
                        checked={isNamespaceIncluded(includedNamespaces, namespace.modUnitId, namespace.namespace)}
                        onChange={() => toggleNamespace(namespace.modUnitId, namespace.namespace)}
                      >
                        {namespace.namespace} · {formatNumber(namespace.entryCount)}
                      </Checkbox>
                    ))}
                  </fieldset>
                )}
              </Disclosure>
            </>
          )}
        </>
      );
      break;
    case "sourceLanguage":
      content = (
        <>
          <h1 className="page-title">{translate("review.sourceLanguageTitle")}</h1>
          <div className="review-summary">
            <div className="review-summary__heading">
              <div>
                <p className="review-summary__label">{translate("review.sourceLanguage")}</p>
                <p className="review-summary__value">{localeDisplayName(sourceLocale)}</p>
              </div>
              <div>
                <p className="review-summary__label">{translate("review.targetLanguage")}</p>
                <p className="review-summary__value">{minecraftLanguageLabel(details.targetLocale)}</p>
              </div>
              {sourceLocales.length > 1 && <TertiaryButton
                aria-controls="source-language-options"
                aria-expanded={showSourceLanguageOptions}
                onClick={() => setShowSourceLanguageOptions((visible) => !visible)}
              >
                {showSourceLanguageOptions ? translate("review.close") : translate("review.change")}
              </TertiaryButton>}
            </div>
            <p className="review-summary__meta">
              {modUnits.length > 1 && <>{translate("review.allTargets")} · </>}
              {selectedLocaleCount === 1
                ? translate("review.sourceLocaleEntries_one", { count: formatNumber(selectedLocaleCount) })
                : translate("review.sourceLocaleEntries", { count: formatNumber(selectedLocaleCount) })}
              {details.recommendedSourceLocale?.toLowerCase() === sourceLocale.toLowerCase() && <span>{translate("review.recommended")}</span>}
            </p>
            <div className="review-summary__selection" id="source-language-options" hidden={!showSourceLanguageOptions || sourceLocales.length < 2}>
              <div className="review-source-select">
                <span>{translate("review.sourceLanguageSelect")}</span>
                <Select
                  aria-label={translate("review.sourceLanguageSelect")}
                  value={sourceLocale}
                  onChange={setSourceLocale}
                  options={sourceLocales.map(([locale]) => ({ value: locale, label: localeDisplayName(locale) }))}
                />
              </div>
            </div>
          </div>
        </>
      );
      break;
    case "existingTarget":
      content = (
        <>
          <h1 className="page-title">{translate("review.existingTargetTitle", { language: targetLanguageName(details.targetLocale) })}</h1>
          <p className="review-description">
            {modUnits.length > 1
              ? existingTargetCount === 1
                ? translate("review.existingTargetCountAll_one", { count: formatNumber(existingTargetCount) })
                : translate("review.existingTargetCountAll", { count: formatNumber(existingTargetCount) })
              : existingTargetCount === 1
                ? translate("review.existingTargetCount_one", { count: formatNumber(existingTargetCount) })
                : translate("review.existingTargetCount", { count: formatNumber(existingTargetCount) })}
          </p>
          <RadioGroup
            className="review-options"
            optionClassName="review-option"
            legend={translate("review.existingTargetLegend", { language: targetLanguageName(details.targetLocale) })}
            name="preserve-japanese"
            value={preserveTarget ? "preserve" : "clear"}
            onChange={(value) => setPreserveTarget(value === "preserve")}
            options={[
              { value: "preserve", label: translate("review.preserveExisting") },
              { value: "clear", label: translate("review.replaceAll") },
            ]}
          />
        </>
      );
      break;
    case "missingKeys":
      content = (
        <>
          <h1 className="page-title">{translate("review.missingKeysTitle")}</h1>
          <p className="review-description">
            {modUnits.length > 1
              ? missingSourceKeyCount === 1
                ? translate("review.missingKeysCountAll_one", { count: formatNumber(missingSourceKeyCount) })
                : translate("review.missingKeysCountAll", { count: formatNumber(missingSourceKeyCount) })
              : missingSourceKeyCount === 1
                ? translate("review.missingKeysCount_one", { count: formatNumber(missingSourceKeyCount) })
                : translate("review.missingKeysCount", { count: formatNumber(missingSourceKeyCount) })}
          </p>
          <RadioGroup
            className="review-options"
            optionClassName="review-option"
            legend={translate("review.missingKeysLegend")}
            name="fill-missing-keys"
            value={fillMissingSourceKeys ? "fill" : "empty"}
            onChange={(value) => setFillMissingSourceKeys(value === "fill")}
            options={[
              { value: "fill", label: translate("review.fillFromOtherLanguages"), disabled: saving },
              { value: "empty", label: translate("review.leaveEmpty"), disabled: saving },
            ]}
          />
        </>
      );
      break;
    case "assets":
      content = (
        <>
          <h1 className="page-title">{translate("review.assetsTitle")}</h1>
          <p className="review-description">
            {modUnits.length > 1
              ? assetSummary.total === 1
                ? translate("review.assetsCountAll_one", { count: formatNumber(assetSummary.total) })
                : translate("review.assetsCountAll", { count: formatNumber(assetSummary.total) })
              : assetSummary.total === 1
                ? translate("review.assetsCount_one", { count: formatNumber(assetSummary.total) })
                : translate("review.assetsCount", { count: formatNumber(assetSummary.total) })}
          </p>
          <dl className="review-details">
            <div><dt>{translate("review.blockModels")}</dt><dd>{formatNumber(assetSummary.blockModels)}</dd></div>
            <div><dt>{translate("review.itemModels")}</dt><dd>{formatNumber(assetSummary.itemModels)}</dd></div>
            <div><dt>{translate("review.entityTextures")}</dt><dd>{formatNumber(assetSummary.entityTextures)}</dd></div>
            <div><dt>{translate("review.geometryModels")}</dt><dd>{formatNumber(assetSummary.geometryModels)}</dd></div>
            <div><dt>{translate("review.unresolvedItems")}</dt><dd>{formatNumber(assetSummary.unresolvedEntries)}</dd></div>
          </dl>
        </>
      );
      break;
  }

  return (
    <section className="focus-page review-screen" aria-label={translate("review.setupLabel")}>
      <StepProgress step={currentStepIndex + 1} total={visibleSteps.length} />
      <div className="review-panel" key={step}>
        {content}
        {currentReviewSummaryLoaded && sameLanguageEntryCount > 0 && (
          <output className="status-message" aria-live="polite">
            {sameLanguageEntryCount === 1
              ? translate("review.sameLanguageWarning_one", {
                count: formatNumber(sameLanguageEntryCount),
                language: targetLanguageName(details.targetLocale),
              })
              : translate("review.sameLanguageWarning", {
                count: formatNumber(sameLanguageEntryCount),
                language: targetLanguageName(details.targetLocale),
              })}
          </output>
        )}
        {reviewSummaryLoading && (
          <output aria-live="polite">
            {/* 件数集計の進捗が未着の間はHTML progressの不確定表示を使う。 */}
            <progress
              className="progress-bar"
              max={100}
              {...(reviewSummaryProgress?.key === currentSelectionKey
                && reviewSummaryProgress.progress.percent != null
                ? { value: reviewSummaryProgress.progress.percent }
                : {})}
              aria-label={translate("review.progress")}
            />
            <span>{reviewSummaryProgress?.key === currentSelectionKey
              ? reviewSummaryProgress.progress.message
              : translate("review.loadingItems")}</span>
          </output>
        )}
        {reviewError && <p className="status-message" role="alert">{reviewError.userMessage}</p>}
      </div>
      <footer className="review-actions">
        <SecondaryButton disabled={saving || reviewSummaryLoading} onClick={goBack}>{translate("review.back")}</SecondaryButton>
        <PrimaryButton data-testid="review-next" disabled={saving || reviewSummaryLoading} onClick={() => void goNext()}>
          {reviewSummaryLoading
            ? translate("review.calculating")
            : reviewError && !reviewSummaryReady
              ? translate("review.retry")
              : saving
                ? (currentStepIndex === visibleSteps.length - 1 ? translate("review.saving") : translate("review.preparing"))
                : currentStepIndex === visibleSteps.length - 1 ? translate("review.startTranslation") : translate("review.next")}
        </PrimaryButton>
      </footer>
    </section>
  );
}
