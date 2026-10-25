import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import { PrimaryButton, SecondaryButton, TertiaryButton } from "../../components/button/Buttons";
import { Checkbox, Disclosure, RadioGroup, Select } from "../../components/Controls";
import { StepProgress } from "../../components/progress/StepProgress";
import { localeDisplayName } from "../../lib/locale";
import { AppCommandError, getReviewAssetSummary, listEntries, saveReviewSettings } from "../../lib/tauri";
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

type ReviewStep = "mod" | "sourceLanguage" | "existingJapanese" | "missingKeys" | "assets";

const EMPTY_ASSET_SUMMARY: ProjectAssetSummaryDto = {
  total: 0,
  blockModels: 0,
  itemModels: 0,
  entityTextures: 0,
  geometryModels: 0,
  unresolvedEntries: 0,
};

const MODPACK_UNIT_NAME = "Modpack 固有（クエスト・KubeJS）";

function selectionKey(sourceLocale: string, namespaces: IncludedNamespaceDto[]) {
  const selections = namespaces
    .map(({ modUnitId, namespace }) => [modUnitId, namespace] as const)
    .sort(([leftId, leftNamespace], [rightId, rightNamespace]) => leftId - rightId || leftNamespace.localeCompare(rightNamespace));
  return JSON.stringify([sourceLocale.toLowerCase(), selections]);
}

function isNamespaceIncluded(selections: IncludedNamespaceDto[], modUnitId: number, namespace: string) {
  return selections.some((selection) => selection.modUnitId === modUnitId && selection.namespace === namespace);
}

function minecraftVersionEvidence(detection: MinecraftVersionDetectionDto | null) {
  if (!detection) return null;
  if (detection.method === "metadata") {
    return detection.rounded
      ? "Mod の対応範囲 " + (detection.originalValue ?? "") + " に近いバージョン"
      : "Mod の対応範囲 " + (detection.originalValue ?? "") + " から判定";
  }
  if (detection.method === "packMcmeta") {
    return "pack.mcmeta の " + (detection.originalValue ?? "値") + " から判定"
      + (detection.rounded ? "（近いバージョン）" : "");
  }
  return "リソース構造から推定" + (detection.rounded ? "（近いバージョン）" : "");
}

export function ReviewScreen() {
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
  const preserveJapanese = useProjectStore((state) => state.preserveJapanese);
  const setPreserveJapanese = useProjectStore((state) => state.setPreserveJapanese);
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
    return [...counts].sort(([left], [right]) => left.localeCompare(right));
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
    if (persistedSelectionsMatch) {
      return {
        existingJapaneseTranslationCount: details.existingJapaneseTranslationCount,
        missingSourceKeysCount: details.missingSourceKeysCount,
        assets: details.assetSummary ?? EMPTY_ASSET_SUMMARY,
      };
    }
    return reviewSummaryState?.key === currentSelectionKey ? reviewSummaryState.summary : null;
  }, [currentSelectionKey, details, persistedSelectionsMatch, reviewSummaryState]);
  const displayedReviewSummary = useMemo(() => currentReviewSummary ?? (details ? {
    existingJapaneseTranslationCount: details.existingJapaneseTranslationCount,
    missingSourceKeysCount: details.missingSourceKeysCount,
    assets: details.assetSummary ?? EMPTY_ASSET_SUMMARY,
  } : null), [currentReviewSummary, details]);
  const visibleSteps = useMemo<ReviewStep[]>(() => {
    const steps: ReviewStep[] = ["mod", "sourceLanguage"];
    if ((displayedReviewSummary?.existingJapaneseTranslationCount ?? 0) > 0) {
      steps.push("existingJapanese");
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
    const fallback = sourceLocales.find(([locale]) => locale.toLowerCase() === recommendedLocale) ?? sourceLocales[0];
    setSourceLocale(fallback[0]);
  }, [details?.recommendedSourceLocale, setSourceLocale, sourceLocale, sourceLocales]);

  useEffect(() => {
    if (!details || persistedSelectionsMatch || reviewSummaryState?.key === currentSelectionKey) return;
    let active = true;
    setReviewError(null);
    void getReviewAssetSummary({
      sourceLocale,
      fallbackPriority: details.fallbackPriority,
      preserveExistingJapanese: details.preserveExistingJapanese,
      fillMissingFromOtherLanguages: details.fillMissingFromOtherLanguages,
      includedNamespaces: [...includedNamespaces],
    }, (progress) => {
      if (active) setReviewSummaryProgress({ key: currentSelectionKey, progress });
    }).then((summary) => {
      if (active) setReviewSummaryState({ key: currentSelectionKey, summary });
    }).catch((error: unknown) => {
      if (!active) return;
      setReviewError(error instanceof AppCommandError
        ? error.appError
        : { code: "unexpected", userMessage: "確認項目を読み込めませんでした。選択を変えて再試行してください。", recoverable: true });
    });
    return () => {
      active = false;
    };
  }, [
    currentSelectionKey,
    details,
    includedNamespaces,
    persistedSelectionsMatch,
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
    setOperationTitle("翻訳を準備しています");
    setOperationProgress({ percent: 0, message: "翻訳設定を保存しています" });
    try {
      await goTo("loading");
      const missingSourceKeysCount = currentReviewSummary?.missingSourceKeysCount ?? 0;
      const savedDetails = await saveReviewSettings({
        sourceLocale,
        fallbackPriority: details.fallbackPriority,
        // 0件で画面を省いても、Preserveの既定選択を設定として維持する。
        preserveExistingJapanese: preserveJapanese,
        fillMissingFromOtherLanguages: missingSourceKeysCount === 0 || fillMissingSourceKeys,
        includedNamespaces: [...includedNamespaces],
      }, setOperationProgress);
      setProjectDetails(savedDetails);
      setOperationProgress({ percent: null, message: "翻訳データを読み込んでいます" });
      const entries = await listEntries(modUnits[0]?.id ?? null);
      useEditorStore.getState().loadEntries(entries, savedDetails.project.id);
      await goTo("editor");
    } catch (error: unknown) {
      setOperationProgress(null);
      setReviewError(error instanceof AppCommandError
        ? error.appError
        : { code: "unexpected", userMessage: "設定を保存できませんでした。もう一度お試しください。", recoverable: true });
      await goTo("review");
    } finally {
      setSaving(false);
      setOperationProgress(null);
    }
  };

  if (!details) {
    return <section className="focus-page review-screen"><output aria-live="polite">プロジェクトを読み込んでいます…</output></section>;
  }

  const currentStepIndex = visibleSteps.indexOf(step);
  const assetSummary = currentReviewSummary?.assets ?? details.assetSummary ?? EMPTY_ASSET_SUMMARY;
  let content;
  switch (step) {
    case "mod":
      content = (
        <>
          <h1 className="page-title">Mod情報を確認</h1>
          <dl className={`review-details${modUnits.length > 1 ? " review-details--mods" : ""}`}>
            {modUnits.length > 1 ? (
              <div>
                <dt>Mod 合計 {modUnits.length.toLocaleString("ja-JP")} 件・名前空間 {details.namespaces.length.toLocaleString("ja-JP")} 件</dt>
                <dd>
                  <ul
                    className="review-mod-list"
                    ref={modListRef}
                    aria-label="プロジェクト内のMod"
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
                          <strong>{unit.sourceKind === "modpack" ? MODPACK_UNIT_NAME : unit.name ?? unit.modId ?? "不明"}</strong>
                          {unit.sourceKind !== "modpack" && (
                            <span className="review-mod-list__version">{unit.version ?? "バージョン不明"} · {unit.sourceFilename}</span>
                          )}
                          <div className="review-mod-list__namespaces">
                            {(namespacesByModUnit.get(unit.id) ?? []).map((namespace) => (
                              <span className="review-mod-list__namespace" key={namespace}>{namespace}</span>
                            ))}
                          </div>
                        </li>
                      );
                    })}
                  </ul>
                </dd>
              </div>
            ) : (
              <>
                <div>
                  <dt>ファイル</dt>
                  <dd>{modUnits[0]?.sourceKind === "modpack"
                    ? MODPACK_UNIT_NAME
                    : modUnits[0]?.sourceFilename ?? details.project.displayName}</dd>
                </div>
                <div><dt>Mod名</dt><dd>{details.mods[0]?.name ?? "不明"}</dd></div>
                <div><dt>バージョン</dt><dd>{details.mods[0]?.version ?? "不明"}</dd></div>
                <div>
                  <dt>Minecraft</dt>
                  <dd>
                    {details.minecraftVersionDetection
                      ? details.minecraftVersionDetection.version
                      : details.mods[0]?.minecraftVersion ?? "不明"}
                    {details.minecraftVersionDetection && (
                      <small className="review-details__hint">{minecraftVersionEvidence(details.minecraftVersionDetection)}</small>
                    )}
                  </dd>
                </div>
                <div><dt>ローダー</dt><dd>{details.mods[0]?.loader ?? "不明"}</dd></div>
                <div>
                  <dt>名前空間</dt>
                  <dd>{details.namespaces.map((item) => item.namespace).join("、") || "不明"}</dd>
                </div>
              </>
            )}
          </dl>
          {modImportFailures.length > 0 && (
            <section aria-label="読み込めなかったJAR" aria-live="polite">
              <p>読み込めなかったJAR</p>
              <ul>
                {modImportFailures.map((failure) => (
                  <li key={failure.filename}>{failure.filename} · {failure.reason}</li>
                ))}
              </ul>
            </section>
          )}
          {nestedModCount > 0 && nestedNamespaces.length > 0 && (
            <>
              <p className="review-note">同梱Mod {nestedModCount}件</p>
              <Disclosure className="review-nested" title="翻訳対象に含める">
                {modUnits.length > 1 ? (
                  <>
                    <div className="review-nested__bulk-actions">
                      <TertiaryButton onClick={() => setAllNestedNamespaces(true)}>全選択</TertiaryButton>
                      <TertiaryButton onClick={() => setAllNestedNamespaces(false)}>全解除</TertiaryButton>
                    </div>
                    <div className="review-nested__groups">
                      {nestedNamespaceGroups.map((group) => (
                        <Disclosure
                          className="review-nested__group"
                          key={group.key}
                          title={`${group.name} · ${group.namespaces.length.toLocaleString("ja-JP")}`}
                        >
                          <fieldset className="review-options">
                            <legend className="visually-hidden">{group.name} の同梱Modの名前空間</legend>
                            {group.namespaces.map((namespace) => (
                              <Checkbox
                                className="review-option"
                                key={`${namespace.modUnitId}:${namespace.namespace}`}
                                name="nested-namespace"
                                checked={isNamespaceIncluded(includedNamespaces, namespace.modUnitId, namespace.namespace)}
                                onChange={() => toggleNamespace(namespace.modUnitId, namespace.namespace)}
                              >
                                {namespace.namespace} · {namespace.entryCount.toLocaleString("ja-JP")}
                              </Checkbox>
                            ))}
                          </fieldset>
                        </Disclosure>
                      ))}
                    </div>
                  </>
                ) : (
                  <fieldset className="review-options">
                    <legend className="visually-hidden">同梱Modの名前空間</legend>
                    {nestedNamespaces.map((namespace) => (
                      <Checkbox
                        className="review-option"
                        key={`${namespace.modUnitId}:${namespace.namespace}`}
                        name="nested-namespace"
                        checked={isNamespaceIncluded(includedNamespaces, namespace.modUnitId, namespace.namespace)}
                        onChange={() => toggleNamespace(namespace.modUnitId, namespace.namespace)}
                      >
                        {namespace.namespace} · {namespace.entryCount.toLocaleString("ja-JP")}
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
          <h1 className="page-title">翻訳元を確認</h1>
          <div className="review-summary">
            <div className="review-summary__heading">
              <div>
                <p className="review-summary__label">翻訳元</p>
                <p className="review-summary__value">{localeDisplayName(sourceLocale)}</p>
              </div>
              {sourceLocales.length > 1 && <TertiaryButton
                aria-controls="source-language-options"
                aria-expanded={showSourceLanguageOptions}
                onClick={() => setShowSourceLanguageOptions((visible) => !visible)}
              >
                {showSourceLanguageOptions ? "閉じる" : "変更"}
              </TertiaryButton>}
            </div>
            <p className="review-summary__meta">
              {modUnits.length > 1 && "全Mod合計 · "}
              {selectedLocaleCount.toLocaleString("ja-JP")} 件
              {details.recommendedSourceLocale?.toLowerCase() === sourceLocale.toLowerCase() && <span>おすすめ</span>}
            </p>
            <div className="review-summary__selection" id="source-language-options" hidden={!showSourceLanguageOptions || sourceLocales.length < 2}>
              <div className="review-source-select">
                <span>翻訳元言語</span>
                <Select
                  aria-label="翻訳元言語"
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
    case "existingJapanese":
      content = (
        <>
          <h1 className="page-title">既存の日本語訳を引き継ぎますか？</h1>
          <p className="review-description">
            {modUnits.length > 1 && "全Mod合計で "}
            {currentReviewSummary?.existingJapaneseTranslationCount.toLocaleString("ja-JP") ?? "0"} 件見つかりました
          </p>
          <RadioGroup
            className="review-options"
            optionClassName="review-option"
            legend="既存の日本語訳"
            name="preserve-japanese"
            value={preserveJapanese ? "preserve" : "clear"}
            onChange={(value) => setPreserveJapanese(value === "preserve")}
            options={[
              { value: "preserve", label: "そのまま引き継ぐ" },
              { value: "clear", label: "すべて新しく翻訳する" },
            ]}
          />
        </>
      );
      break;
    case "missingKeys":
      content = (
        <>
          <h1 className="page-title">不足キーを補完しますか？</h1>
          <p className="review-description">
            {modUnits.length > 1 && "全Mod合計で "}
            翻訳元にないキーが {currentReviewSummary?.missingSourceKeysCount.toLocaleString("ja-JP") ?? "0"} 件あります
          </p>
          <RadioGroup
            className="review-options"
            optionClassName="review-option"
            legend="不足キーの扱い"
            name="fill-missing-keys"
            value={fillMissingSourceKeys ? "fill" : "empty"}
            onChange={(value) => setFillMissingSourceKeys(value === "fill")}
            options={[
              { value: "fill", label: "他の言語から補完する", disabled: saving },
              { value: "empty", label: "空欄のままにする", disabled: saving },
            ]}
          />
        </>
      );
      break;
    case "assets":
      content = (
        <>
          <h1 className="page-title">プレビュー用アセット</h1>
          <p className="review-description">
            {modUnits.length > 1 && "全Mod合計で "}
            プレビューに使えるアセットを {assetSummary.total.toLocaleString("ja-JP")} 件見つけました
          </p>
          <dl className="review-details">
            <div><dt>ブロックモデル</dt><dd>{(assetSummary?.blockModels ?? 0).toLocaleString("ja-JP")}</dd></div>
            <div><dt>アイテムモデル</dt><dd>{(assetSummary?.itemModels ?? 0).toLocaleString("ja-JP")}</dd></div>
            <div><dt>エンティティのテクスチャ</dt><dd>{(assetSummary?.entityTextures ?? 0).toLocaleString("ja-JP")}</dd></div>
            <div><dt>ジオメトリモデル</dt><dd>{(assetSummary?.geometryModels ?? 0).toLocaleString("ja-JP")}</dd></div>
            <div><dt>未解決の項目</dt><dd>{(assetSummary?.unresolvedEntries ?? 0).toLocaleString("ja-JP")}</dd></div>
          </dl>
        </>
      );
      break;
  }

  return (
    <section className="focus-page review-screen" aria-label="セットアップ確認">
      <StepProgress step={currentStepIndex + 1} total={visibleSteps.length} />
      <div className="review-panel" key={step}>
        {content}
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
              aria-label="確認項目の進行状況"
            />
            <span>{reviewSummaryProgress?.key === currentSelectionKey
              ? reviewSummaryProgress.progress.message
              : "確認項目を読み込んでいます"}</span>
          </output>
        )}
        {reviewError && <p className="status-message" role="alert">{reviewError.userMessage}</p>}
      </div>
      <footer className="review-actions">
        <SecondaryButton disabled={saving || reviewSummaryLoading} onClick={goBack}>戻る</SecondaryButton>
        <PrimaryButton data-testid="review-next" disabled={saving || reviewSummaryLoading} onClick={() => void goNext()}>
          {reviewSummaryLoading
            ? "集計中…"
            : reviewError && !reviewSummaryReady
              ? "再試行"
              : saving
                ? (currentStepIndex === visibleSteps.length - 1 ? "保存中…" : "準備中…")
                : currentStepIndex === visibleSteps.length - 1 ? "翻訳を始める" : "次へ"}
        </PrimaryButton>
      </footer>
    </section>
  );
}
