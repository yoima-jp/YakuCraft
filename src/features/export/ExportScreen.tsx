import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { join as joinPath } from "@tauri-apps/api/path";

import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { Disclosure, NumberInput, RadioGroup, Select } from "../../components/Controls";
import { AppCommandError, getProjectSummary, listModUnits } from "../../lib/tauri";
import { logFrontendError } from "../../lib/errorLogging";
import { openFileDialog, saveFileDialog } from "../../lib/dialogs";
import type { ExportKind, ManualPackFormatDto } from "../../types/export";
import type { MinecraftVersionDetectionDto, ProjectDetailsDto } from "../../types/project";
import { useAppStore } from "../../stores/appStore";
import { useProjectStore } from "../../stores/projectStore";
import { useSettingsStore } from "../../stores/settingsStore";
import { setExportRunDetails } from "./completionData";
import { displayPath } from "./pathDisplay";
import {
  ExportCommandError,
  exportSelectedLanguageFile,
  exportSelectedLanguageFiles,
  exportSelectedMergedResourcePack,
  exportSelectedModifiedJars,
  exportModpackFiles,
  exportShaderpacks,
  getExportOverview,
  restoreModpackFile,
  restoreModifiedJar,
  validateSelectedProject,
  type ExportIssueDto,
  type SelectedExportValidationDto,
  type ExportOverviewDto,
  type ModExportResultDto,
  type FileDeploymentDto,
} from "./exportApi";
import "./ExportScreen.css";

const EXPORT_OPTIONS: Array<{ value: ExportKind; label: string; description: string }> = [
  { value: "resourcePack", label: "リソースパック", description: "複数のmodを1つのZIPにまとめる" },
  { value: "modifiedJar", label: "Mod JARに埋め込む", description: "日本語ファイルをJARに追加する" },
  { value: "languageFile", label: "言語ファイル", description: "言語ファイルだけを書き出す" },
  { value: "modpackFiles", label: "Modpack ファイル", description: "クエスト・KubeJS の翻訳を書き出す" },
  { value: "shaderpackZip", label: "日本語化したシェーダーパック", description: "元パックを残して別名 ZIP を作る" },
];

function safeFileComponent(value: string) {
  const component = [...value]
    .map((character) => {
      const code = character.charCodeAt(0);
      const isControl = code <= 0x1f || (code >= 0x7f && code <= 0x9f);
      if (/[<>:"/\\|?*]/.test(character)) return "_";
      return isControl ? "" : character;
    })
    .join("")
    .replace(/^[.\s]+|[.\s]+$/g, "");
  return component || "mod";
}

function languageFileNameForNamespace(details: ProjectDetailsDto, namespace: string) {
  const languages = details.languages.filter((language) => language.namespace === namespace);
  const japanese = languages.find((language) => language.locale.toLowerCase() === "ja_jp");
  if (japanese) return japanese.archivePath.split(/[\\/]/).at(-1) ?? "ja_jp.json";

  const source = languages.find((language) => language.locale.toLowerCase() === details.sourceLocale.toLowerCase());
  if (!source) return "ja_jp.json";
  const sourceName = source.archivePath.split(/[\\/]/).at(-1) ?? "en_us.json";
  const dot = sourceName.lastIndexOf(".");
  if (dot < 0) return "ja_jp.json";
  const stem = sourceName.slice(0, dot);
  const region = stem.slice(stem.lastIndexOf("_") + 1);
  const upperRegion = region.length > 0 && !/[a-z]/.test(region);
  return `ja_${upperRegion ? "JP" : "jp"}${sourceName.slice(dot)}`;
}

function errorMessage(error: unknown) {
  void logFrontendError(error, "export screen");
  if (error instanceof ExportCommandError || error instanceof AppCommandError) return error.message;
  return "処理に失敗しました。もう一度お試しください。";
}

function isProjectChangedError(error: unknown) {
  return error instanceof ExportCommandError && error.code === "project_changed";
}

function minecraftVersionEvidence(detection: MinecraftVersionDetectionDto | null | undefined) {
  if (!detection) return null;
  if (detection.method === "metadata") {
    return detection.rounded
      ? `Mod の対応範囲 ${detection.originalValue ?? ""} に近いバージョン`
      : `Mod の対応範囲 ${detection.originalValue ?? ""} から判定`;
  }
  if (detection.method === "packMcmeta") {
    return `pack.mcmeta の ${detection.originalValue ?? "値"} から判定${detection.rounded ? "（近いバージョン）" : ""}`;
  }
  return `リソース構造から推定${detection.rounded ? "（近いバージョン）" : ""}`;
}

function unitTitle(unit: { sourceKind?: "jar" | "modpack" | "shaderpack"; sourceFilename: string; name: string | null; version?: string | null }) {
  if (unit.sourceKind === "modpack") return "Modpack 固有（クエスト・KubeJS）";
  if (unit.sourceKind === "shaderpack") return `シェーダーパック: ${unit.sourceFilename}`;
  const name = unit.name?.trim() || unit.sourceFilename;
  return unit.version?.trim() ? `${name} ${unit.version}` : name;
}

function isJarUnit(unit: { sourceKind?: "jar" | "modpack" | "shaderpack" }) {
  return unit.sourceKind === undefined || unit.sourceKind === "jar";
}

function ModUnitSelector({
  units,
  selectedIds,
  onChange,
}: {
  units: ReturnType<typeof useProjectStore.getState>["modUnits"];
  selectedIds: number[];
  onChange: (ids: number[]) => void;
}) {
  const [query, setQuery] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const selected = useMemo(() => new Set(selectedIds), [selectedIds]);
  const filteredUnits = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    if (!normalized) return units;
    return units.filter((unit) => unitTitle(unit).toLocaleLowerCase().includes(normalized)
      || unit.sourceFilename.toLocaleLowerCase().includes(normalized));
  }, [query, units]);
  // TanStack Virtual intentionally exposes an imperative viewport object that React Compiler cannot memoize.
  // oxlint-disable-next-line react/incompatible-library
  const virtualizer = useVirtualizer({
    count: filteredUnits.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 46,
    overscan: 8,
  });

  const toggle = (id: number, checked: boolean) => {
    onChange(checked ? [...selectedIds, id] : selectedIds.filter((selectedId) => selectedId !== id));
  };

  return (
    <fieldset className="export-unit-selector">
      <legend>対象</legend>
      <div className="export-unit-selector__toolbar">
        <input
          type="search"
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          aria-label="対象を検索"
          placeholder="名前で検索"
          value={query}
          onChange={(event) => setQuery(event.currentTarget.value)}
        />
        <SecondaryButton onClick={() => onChange(units.map((unit) => unit.id))}>全選択</SecondaryButton>
        <SecondaryButton onClick={() => onChange([])}>全解除</SecondaryButton>
      </div>
      <div className="export-unit-selector__list" ref={scrollRef}>
        <div className="export-unit-selector__spacer" style={{ height: virtualizer.getTotalSize() }}>
          {virtualizer.getVirtualItems().map((item) => {
            const unit = filteredUnits[item.index];
            if (!unit) return null;
            return (
              <label
                className="export-unit-selector__row"
                key={unit.id}
                style={{ transform: `translateY(${item.start}px)` }}
              >
                <input
                  type="checkbox"
                  checked={selected.has(unit.id)}
                  onChange={(event) => toggle(unit.id, event.currentTarget.checked)}
                />
                <span>{unitTitle(unit)}</span>
              </label>
            );
          })}
        </div>
      </div>
      <small>{selectedIds.length.toLocaleString("ja-JP")} / {units.length.toLocaleString("ja-JP")}</small>
    </fieldset>
  );
}

async function chooseDirectory(defaultPath: string) {
  const selected = await openFileDialog({
    directory: true,
    multiple: false,
    ...(defaultPath ? { defaultPath } : {}),
  });
  return Array.isArray(selected) ? selected[0] ?? null : selected;
}

function mergedPackName(projectName: string, instancePath: string | null) {
  const instanceName = instancePath?.replace(/[\\/]+$/, "").split(/[\\/]/).at(-1)?.trim();
  const name = instanceName || projectName.trim();
  return !name || /^\d+ 個の mod$/.test(name) ? "YakuCraft" : name;
}

function mergeOutputFileName(
  units: ReturnType<typeof useProjectStore.getState>["modUnits"],
  selectedIds: number[],
  projectName: string,
  instancePath: string | null,
) {
  if (selectedIds.length > 1) return `${safeFileComponent(mergedPackName(projectName, instancePath))}-JP_EX.zip`;
  const unit = units.find((candidate) => candidate.id === selectedIds[0]);
  const sourceStem = (unit?.sourceFilename ?? "mod.jar").replace(/\.jar$/i, "");
  const name = safeFileComponent(unit?.name?.trim() || sourceStem);
  const version = unit?.version?.trim();
  return `${name}${version ? `-${safeFileComponent(version)}` : ""}-JP_EX.zip`;
}

type ValidationVirtualRow =
  | { kind: "missing"; id: string; modName: string; count: number }
  | { kind: "mod"; id: string; modName: string }
  | {
    kind: "placeholder";
    id: string;
    severity: "warning" | "error";
    namespace: string;
    key: string;
    label: string;
    sourceText: string;
    targetText: string;
  };

function VirtualValidationList({ rows, label }: { rows: ValidationVirtualRow[]; label: string }) {
  const scrollRef = useRef<HTMLDivElement>(null);
  // TanStack Virtual intentionally exposes an imperative viewport object that React Compiler cannot memoize.
  // oxlint-disable-next-line react/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (index) => (rows[index]?.kind === "placeholder" ? 112 : 40),
    overscan: 6,
  });

  return (
    <div
      className="export-validation__list"
      ref={scrollRef}
      style={{ height: virtualizer.getTotalSize() }}
    >
      {/* 可視行だけを描画しても、内部のリストで全件分のスクロール範囲を確保する。 */}
      <ul className="export-validation__items" aria-label={label} style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          const style = { transform: `translateY(${item.start}px)` };
          if (row.kind === "mod") {
            return <li className="export-validation__row export-validation__row--mod" key={row.id} style={style}>{row.modName}</li>;
          }
          if (row.kind === "missing") {
            return (
              <li className="export-validation__row export-validation__row--missing" key={row.id} style={style}>
                <strong>{row.modName}</strong>
                <span>{row.count.toLocaleString("ja-JP")} 件</span>
              </li>
            );
          }
          return (
            <li
              className={`export-validation__row export-validation__row--placeholder ${row.severity === "error" ? "export-validation__row--placeholder-error" : ""}`}
              key={row.id}
              style={style}
            >
              <code className="selectable" title={row.label}>
                {row.severity === "error" ? "エラー" : "注意"} · {row.label}
              </code>
              <p><strong>原文</strong><span className="selectable" title={row.sourceText}>{row.sourceText}</span></p>
              <p><strong>訳文</strong><span className="selectable" title={row.targetText}>{row.targetText}</span></p>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export function ExportScreen() {
  const details = useProjectStore((state) => state.details);
  const projectId = useProjectStore((state) => state.details?.project.id ?? null);
  const modUnits = useProjectStore((state) => state.modUnits);
  const setModUnits = useProjectStore((state) => state.setModUnits);
  const setProjectDetails = useProjectStore((state) => state.setProjectDetails);
  const goTo = useAppStore((state) => state.goTo);
  const setExportCompletion = useAppStore((state) => state.setExportCompletion);
  const defaultExportDirectory = useSettingsStore((state) => state.value<string>("general.defaultExportDirectory").trim());
  const autoOpenOutputFolder = useSettingsStore((state) => state.value<boolean>("general.autoOpenOutputFolder"));
  const [overview, setOverview] = useState<ExportOverviewDto | null>(null);
  const [loadedProjectId, setLoadedProjectId] = useState<string | null>(null);
  const [kind, setKind] = useState<ExportKind>("resourcePack");
  const [selectedIds, setSelectedIds] = useState<number[]>([]);
  const [minecraftVersion, setMinecraftVersion] = useState("");
  const [manualMinecraftVersion, setManualMinecraftVersion] = useState("");
  const [manualFormatMajor, setManualFormatMajor] = useState("");
  const [manualFormatMinor, setManualFormatMinor] = useState("");
  const [namespace, setNamespace] = useState("");
  const [destination, setDestination] = useState<"instance" | "folder">("folder");
  const [replaceInPlace, setReplaceInPlace] = useState(false);
  const [validation, setValidation] = useState<SelectedExportValidationDto | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [missingDetailsOpen, setMissingDetailsOpen] = useState(false);
  const [placeholderDetailsOpen, setPlaceholderDetailsOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [restoringId, setRestoringId] = useState<number | null>(null);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 強制書き出しの許可は、警告を出したときのプロジェクト・形式・対象と書式エラー件数に紐付ける。
  const [forcePermit, setForcePermit] = useState<{ target: string; count: number } | null>(null);
  const placeholderDetailsRef = useRef<HTMLDetailsElement>(null);
  const blockingIssuesRef = useRef<HTMLUListElement>(null);
  const focusIssue = useRef<"placeholder" | "blocking" | null>(null);
  const latestValidationRequest = useRef(0);
  const screenActive = useRef(false);

  useEffect(() => {
    if (!focusIssue.current) return;
    const target = focusIssue.current === "placeholder" ? placeholderDetailsRef.current : blockingIssuesRef.current;
    if (target) {
      if (target instanceof HTMLDetailsElement) target.open = true;
      target.scrollIntoView?.({ block: "nearest" });
      target.focus();
    }
    focusIssue.current = null;
  }, [error, validation]);

  const isCurrentProject = useCallback((expectedProjectId: string) => (
    screenActive.current && useProjectStore.getState().details?.project.id === expectedProjectId
  ), []);

  useEffect(() => {
    screenActive.current = true;
    return () => {
      screenActive.current = false;
      latestValidationRequest.current += 1;
    };
  }, []);

  // Reactのデータ取得例に沿ってcleanupで古い要求を無効化し、要求時のIDも照合して共有storeを守る。
  // https://react.dev/reference/react/useEffect#fetching-data-with-effects
  useEffect(() => {
    if (!projectId) {
      // Project-owned UI state must be cleared before this screen can be reused for another project.
      // oxlint-disable-next-line react/set-state-in-effect
      setLoadedProjectId(null);
      setBusy(false);
      setRestoringId(null);
      setRestoreError(null);
      return;
    }
    let active = true;
    latestValidationRequest.current += 1;
    // Keep A's fields hidden while this effect loads the project that replaced it.
    // oxlint-disable-next-line react/set-state-in-effect
    setLoadedProjectId(null);
    setOverview(null);
    setValidation(null);
    setValidationError(null);
    setBusy(false);
    setRestoringId(null);
    setRestoreError(null);
    setError(null);
    void (async () => {
      const [summary, units, exportOverview] = await Promise.all([
        getProjectSummary(),
        listModUnits(),
        getExportOverview(projectId),
      ]);
      if (!active || !isCurrentProject(projectId) || summary.project.id !== projectId) return;
      setProjectDetails(summary);
      setModUnits(units);
      setOverview(exportOverview);
      setSelectedIds(units.map((unit) => unit.id));
      const shaderpackOnly = units.length > 0 && units.every((unit) => unit.sourceKind === "shaderpack");
      setDestination(shaderpackOnly ? "folder" : exportOverview.instancePath ? "instance" : "folder");
      setKind(shaderpackOnly
        ? "shaderpackZip"
        : units.every((unit) => !isJarUnit(unit)) && units.some((unit) => unit.sourceKind === "modpack")
          ? "modpackFiles"
          : "resourcePack");
      const versionEstimate = units.find((unit) => unit.minecraftVersion?.trim())?.minecraftVersion;
      setMinecraftVersion(versionEstimate || summary.minecraftVersionDetection?.version || "");
      setNamespace(summary.includedNamespaces[0]?.namespace ?? summary.namespaces[0]?.namespace ?? "");
      setLoadedProjectId(projectId);
    })().catch((cause: unknown) => {
      if (active && isCurrentProject(projectId) && !isProjectChangedError(cause)) {
        setError(errorMessage(cause));
      }
    });
    return () => {
      active = false;
      latestValidationRequest.current += 1;
    };
  }, [isCurrentProject, projectId, setModUnits, setProjectDetails]);

  const effectiveMinecraftVersion = manualMinecraftVersion.trim() || minecraftVersion;
  const manualPackFormat: ManualPackFormatDto | undefined = useMemo(() => (
    manualMinecraftVersion.trim()
      && /^\d+$/.test(manualFormatMajor)
      && /^\d+$/.test(manualFormatMinor)
      ? { major: Number(manualFormatMajor), minor: Number(manualFormatMinor) }
      : undefined
  ), [manualFormatMajor, manualFormatMinor, manualMinecraftVersion]);
  const selectedIdSet = useMemo(() => new Set(selectedIds), [selectedIds]);
  const hasJarUnit = modUnits.some(isJarUnit);
  const hasModpackUnit = modUnits.some((unit) => unit.sourceKind === "modpack");
  const hasShaderpackUnit = modUnits.some((unit) => unit.sourceKind === "shaderpack");
  const selectableUnits = kind === "modifiedJar"
    ? modUnits.filter(isJarUnit)
    : kind === "shaderpackZip"
      ? modUnits.filter((unit) => unit.sourceKind === "shaderpack")
      // 言語ファイルはシェーダーの lang 単体出力にも使うので、全ユニットを対象にする。
      // 全選択で selectedIds が一覧の内容に置き換わるため、ここで外すと後から再選択できなくなる。
      : kind === "languageFile"
        ? modUnits
        // シェーダーの lang はパック自体に書き込む方式なので、リソースパックなどの対象からは外す。
        // Modpack ユニットは KubeJS の翻訳をリソースパックにも含められるため、従来どおり残す。
        : modUnits.filter((unit) => unit.sourceKind !== "shaderpack");
  const selectedJarIds = modUnits
    .filter((unit) => selectedIdSet.has(unit.id) && isJarUnit(unit))
    .map((unit) => unit.id);
  const selectedShaderpackIds = modUnits
    .filter((unit) => selectedIdSet.has(unit.id) && unit.sourceKind === "shaderpack")
    .map((unit) => unit.id);
  const selectedFtbQuestEntries = modUnits
    .filter((unit) => selectedIdSet.has(unit.id) && unit.sourceKind === "modpack")
    .reduce((total, unit) => total + (unit.ftbQuestEntries ?? 0), 0);
  const includedNamespaces = useMemo(() => [...new Set(
    (details?.includedNamespaces ?? [])
      .filter((selection) => selectedIdSet.has(selection.modUnitId))
      .map((selection) => selection.namespace),
  )], [details?.includedNamespaces, selectedIdSet]);
  const selectedNamespace = includedNamespaces.includes(namespace) ? namespace : (includedNamespaces[0] ?? "");
  const versionOptions = useMemo(() => [...new Set([
    ...(details?.supportedMinecraftVersions ?? []),
    ...modUnits.map((unit) => unit.minecraftVersion?.trim() ?? "").filter(Boolean),
  ])], [details?.supportedMinecraftVersions, modUnits]);
  const eligibleForInPlace = Boolean(overview?.instancePath)
    && overview?.units.some((unit) => selectedIdSet.has(unit.id)
      && isJarUnit(unit) && unit.originalExists);

  const validate = useCallback(async (
    nextKind = kind,
    version = effectiveMinecraftVersion,
    selectedNamespace = namespace,
    format = manualPackFormat,
    ids = selectedIds,
  ) => {
    if (
      !projectId
      || loadedProjectId !== projectId
      || ids.length === 0
      || !isCurrentProject(projectId)
    ) return null;
    const requestId = ++latestValidationRequest.current;
    try {
      const result = await validateSelectedProject(
        projectId,
        nextKind,
        ids,
        nextKind === "resourcePack" ? version : undefined,
        nextKind === "languageFile" ? selectedNamespace : undefined,
        nextKind === "resourcePack" ? format : undefined,
      );
      if (requestId !== latestValidationRequest.current || !isCurrentProject(projectId)) return null;
      setValidationError(null);
      setValidation(result);
      return result;
    } catch (cause: unknown) {
      if (
        requestId === latestValidationRequest.current
        && isCurrentProject(projectId)
        && !isProjectChangedError(cause)
      ) {
        setValidationError(errorMessage(cause));
      }
      return null;
    }
  }, [effectiveMinecraftVersion, isCurrentProject, kind, loadedProjectId, manualPackFormat, namespace, projectId, selectedIds]);

  useEffect(() => {
    if (
      !projectId
      || loadedProjectId !== projectId
      || selectedIds.length === 0
      || !isCurrentProject(projectId)
    ) {
      latestValidationRequest.current += 1;
      // A response from the previous selection/project must not remain visible during a new request.
      // oxlint-disable-next-line react/set-state-in-effect
      setValidation(null);
      setValidationError(null);
      return;
    }
    let active = true;
    const requestId = ++latestValidationRequest.current;
    void validateSelectedProject(
      projectId,
      kind,
      selectedIds,
      kind === "resourcePack" ? effectiveMinecraftVersion : undefined,
      kind === "languageFile" ? selectedNamespace : undefined,
      kind === "resourcePack" ? manualPackFormat : undefined,
    )
      .then((result) => {
        if (!active || requestId !== latestValidationRequest.current || !isCurrentProject(projectId)) return;
        setValidationError(null);
        setValidation(result);
      })
      .catch((cause: unknown) => {
        if (
          active
          && requestId === latestValidationRequest.current
          && isCurrentProject(projectId)
          && !isProjectChangedError(cause)
        ) {
          setValidationError(errorMessage(cause));
        }
      });
    return () => {
      active = false;
      if (requestId === latestValidationRequest.current) latestValidationRequest.current += 1;
    };
  }, [effectiveMinecraftVersion, isCurrentProject, kind, loadedProjectId, manualPackFormat, projectId, selectedIds, selectedNamespace]);

  const refreshOverview = useCallback(async (expectedProjectId: string) => {
    const nextOverview = await getExportOverview(expectedProjectId);
    if (!isCurrentProject(expectedProjectId)) return false;
    setOverview(nextOverview);
    return true;
  }, [isCurrentProject]);

  const restoreJar = async (replacementId: number) => {
    const startingProjectId = projectId;
    if (!startingProjectId || loadedProjectId !== startingProjectId || !isCurrentProject(startingProjectId)) return;
    setRestoringId(replacementId);
    setRestoreError(null);
    try {
      let outcome = await restoreModifiedJar(startingProjectId, replacementId, false);
      if (!isCurrentProject(startingProjectId)) return;
      if (outcome.status === "confirmationRequired") {
        const confirmed = window.confirm(
          "このJARは書き出し後に変更されています。バックアップで現在のJARを上書きして元に戻しますか？",
        );
        if (!confirmed) return;
        if (!isCurrentProject(startingProjectId)) return;
        outcome = await restoreModifiedJar(startingProjectId, replacementId, true);
        if (!isCurrentProject(startingProjectId)) return;
      }
      if (outcome.status === "restored") {
        await refreshOverview(startingProjectId);
        if (!isCurrentProject(startingProjectId)) return;
      }
    } catch (cause: unknown) {
      if (!isProjectChangedError(cause) && isCurrentProject(startingProjectId)) {
        setRestoreError(errorMessage(cause));
      }
    } finally {
      if (isCurrentProject(startingProjectId)) setRestoringId(null);
    }
  };

  const restoreFile = async (deploymentId: number) => {
    const startingProjectId = projectId;
    if (!startingProjectId || loadedProjectId !== startingProjectId || !isCurrentProject(startingProjectId)) return;
    setRestoringId(deploymentId);
    setRestoreError(null);
    try {
      await restoreModpackFile(startingProjectId, deploymentId);
      if (!isCurrentProject(startingProjectId)) return;
      await refreshOverview(startingProjectId);
      if (!isCurrentProject(startingProjectId)) return;
    } catch (cause: unknown) {
      if (!isProjectChangedError(cause) && isCurrentProject(startingProjectId)) {
        setRestoreError(errorMessage(cause));
      }
    } finally {
      if (isCurrentProject(startingProjectId)) setRestoringId(null);
    }
  };

  const exportTarget = `${projectId ?? ""}:${kind}:${selectedIds.join(",")}`;
  const beginExport = async (allowedCount: number | null = null) => {
    const startingProjectId = projectId;
    if (
      !details
      || !startingProjectId
      || loadedProjectId !== startingProjectId
      || selectedIds.length === 0
      || !isCurrentProject(startingProjectId)
    ) return;
    setBusy(true);
    setError(null);
    setForcePermit(null);
    try {
      const result = await validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat, selectedIds);
      if (!result || !isCurrentProject(startingProjectId)) return;
      const blockingErrors = result.issues.filter((issue) =>
        issue.severity === "error" && issue.code !== "modUnavailable",
      );
      const formatErrorCount = blockingErrors.filter((issue) => issue.code === "placeholderError").length;
      if (blockingErrors.length > 0 && kind !== "modifiedJar") {
        const onlyFormatErrors = formatErrorCount === blockingErrors.length;
        if (!onlyFormatErrors || allowedCount !== formatErrorCount) {
          setError(onlyFormatErrors
            ? `書式エラーが ${formatErrorCount.toLocaleString("ja-JP")} 件あります。該当の訳はゲーム内で正しく表示されないことがあります`
            : "確認事項を直すと書き出せます");
          if (onlyFormatErrors) setForcePermit({ target: exportTarget, count: formatErrorCount });
          focusIssue.current = onlyFormatErrors ? "placeholder" : "blocking";
          return;
        }
      }

      const defaultName = kind === "resourcePack"
        ? mergeOutputFileName(
          modUnits,
          selectedIds.filter((id) => details.includedNamespaces.some((item) => item.modUnitId === id)),
          details.project.displayName,
          overview?.instancePath ?? null,
        )
        : kind === "modifiedJar"
          ? "改変済みmod"
          : kind === "modpackFiles"
            ? "Modpack 翻訳"
            : kind === "shaderpackZip"
              ? `${safeFileComponent(modUnits.find((unit) => selectedShaderpackIds.includes(unit.id))?.sourceFilename ?? "shaderpack")}-ja.zip`
              : "言語ファイル";
      const defaultPath = defaultExportDirectory
        ? await joinPath(defaultExportDirectory, defaultName)
        : defaultName;
      let outputPaths: string[] = [];
      let modResults: ModExportResultDto[] = [];
      let conflicts: Array<{ namespace: string; key: string; keptFrom: string; droppedFrom: string }> = [];
      let directlyInstalled = false;
      let inPlaceReplacement = false;
      let translatedCount = kind === "modpackFiles" ? details.counts.translated : result.counts.translated;
      let missingCount = kind === "modpackFiles" ? details.counts.missing : result.counts.missing;

      if (kind === "modpackFiles") {
        let outputPath: string | null = null;
        if (destination === "folder") {
          outputPath = await saveFileDialog({ defaultPath: `${defaultPath}.zip`, filters: [{ name: "ZIP", extensions: ["zip"] }] });
          if (!isCurrentProject(startingProjectId)) return;
          if (!outputPath) return;
        } else if (!window.confirm(
          "Minecraft を終了してから配置してください。既存ファイルはバックアップされ、書き出し画面から戻せます。",
        )) {
          return;
        }
        let outcome = await exportModpackFiles(
          startingProjectId, selectedIds, outputPath, destination === "instance", false,
        );
        if (!isCurrentProject(startingProjectId)) return;
        if (outcome.status === "overwriteRequired") {
          const confirmed = window.confirm(
            `既存のファイルがあります。上書きしますか？\n${displayPath(outcome.targetPath)}`,
          );
          if (!confirmed) return;
          if (!isCurrentProject(startingProjectId)) return;
          outcome = await exportModpackFiles(
            startingProjectId, selectedIds, outputPath, destination === "instance", true,
          );
          if (!isCurrentProject(startingProjectId)) return;
        }
        if (outcome.status !== "completed") return;
        outputPaths = outcome.outputPath ? [outcome.outputPath] : [];
        directlyInstalled = outcome.directlyInstalled;
        if (directlyInstalled) {
          await refreshOverview(startingProjectId);
          if (!isCurrentProject(startingProjectId)) return;
        }
      } else if (kind === "shaderpackZip") {
        const outputDirectory = destination === "folder"
          ? await chooseDirectory(defaultExportDirectory)
          : null;
        if (!isCurrentProject(startingProjectId)) return;
        if (destination === "folder" && !outputDirectory) return;
        if (destination === "instance" && !window.confirm(
          "Minecraft を終了してから配置してください。既存ファイルはバックアップされ、書き出し画面から戻せます。",
        )) return;
        let outcome = await exportShaderpacks(
          startingProjectId,
          selectedShaderpackIds,
          outputDirectory ?? null,
          destination === "instance",
          false,
        );
        if (!isCurrentProject(startingProjectId)) return;
        if (outcome.status === "overwriteRequired") {
          const confirmed = window.confirm(
            `同じ名前のファイルがあります。上書きしますか？\n${displayPath(outcome.targetPath)}`,
          );
          if (!confirmed) return;
          if (!isCurrentProject(startingProjectId)) return;
          outcome = await exportShaderpacks(
            startingProjectId,
            selectedShaderpackIds,
            outputDirectory ?? null,
            destination === "instance",
            true,
          );
          if (!isCurrentProject(startingProjectId)) return;
        }
        if (outcome.status !== "completed") return;
        outputPaths = outcome.outputPaths;
        directlyInstalled = outcome.directlyInstalled;
        modResults = outcome.modResults;
        if (directlyInstalled) {
          await refreshOverview(startingProjectId);
          if (!isCurrentProject(startingProjectId)) return;
        }
      } else if (kind === "resourcePack") {
        let outputPath: string | null = null;
        if (destination === "folder") {
          outputPath = await saveFileDialog({ defaultPath, filters: [{ name: "ZIP", extensions: ["zip"] }] });
          if (!isCurrentProject(startingProjectId)) return;
          if (!outputPath) return;
        }
        let outcome = await exportSelectedMergedResourcePack(
          startingProjectId,
          selectedIds,
          outputPath,
          effectiveMinecraftVersion,
          manualPackFormat,
          destination === "instance",
          false,
        );
        if (!isCurrentProject(startingProjectId)) return;
        if (outcome.status === "overwriteRequired") {
          const confirmed = window.confirm("同じ名前のリソースパックがあります。上書きしますか？");
          if (!confirmed) return;
          if (!isCurrentProject(startingProjectId)) return;
          outcome = await exportSelectedMergedResourcePack(
            startingProjectId,
            selectedIds,
            outputPath,
            effectiveMinecraftVersion,
            manualPackFormat,
            destination === "instance",
            true,
          );
          if (!isCurrentProject(startingProjectId)) return;
        }
        if (outcome.status !== "completed") return;
        outputPaths = [outcome.outputPath];
        conflicts = outcome.conflicts;
        modResults = outcome.modResults;
        directlyInstalled = outcome.directlyInstalled;
      } else if (kind === "modifiedJar") {
        const outputDirectory = replaceInPlace ? "" : await chooseDirectory(defaultExportDirectory);
        if (!isCurrentProject(startingProjectId)) return;
        if (!replaceInPlace && !outputDirectory) return;
        modResults = await exportSelectedModifiedJars(startingProjectId, selectedIds, outputDirectory ?? "", replaceInPlace);
        if (!isCurrentProject(startingProjectId)) return;
        outputPaths = modResults.flatMap((item) => item.outputPath ? [item.outputPath] : []);
        inPlaceReplacement = replaceInPlace;
        try {
          await refreshOverview(startingProjectId);
          if (!isCurrentProject(startingProjectId)) return;
        } catch (cause: unknown) {
          if (!isProjectChangedError(cause) && isCurrentProject(startingProjectId)) {
            setRestoreError(errorMessage(cause));
          }
        }
      } else {
        if (modUnits.length === 1 && selectedIds.length === 1) {
          // 単体modは従来どおりファイル名を指定して出力し、mod別サブフォルダを挟まない。
          const fileName = languageFileNameForNamespace(details, selectedNamespace);
          const defaultPath = defaultExportDirectory
            ? await joinPath(defaultExportDirectory, fileName)
            : fileName;
          const outputPath = await saveFileDialog({
            defaultPath,
            filters: [{ name: "言語ファイル", extensions: ["json", "lang"] }],
          });
          if (!isCurrentProject(startingProjectId)) return;
          if (!outputPath) return;
          outputPaths = [await exportSelectedLanguageFile(startingProjectId, outputPath, selectedNamespace)];
          if (!isCurrentProject(startingProjectId)) return;
        } else {
          const outputDirectory = await chooseDirectory(defaultExportDirectory);
          if (!isCurrentProject(startingProjectId)) return;
          if (!outputDirectory) return;
          modResults = await exportSelectedLanguageFiles(startingProjectId, selectedIds, outputDirectory, selectedNamespace);
          if (!isCurrentProject(startingProjectId)) return;
          outputPaths = modResults.flatMap((item) => item.outputPath ? [item.outputPath] : []);
        }
      }

      setExportRunDetails({ kind, directInstall: directlyInstalled, inPlaceReplacement, outputPaths, modResults, conflicts, placeholderErrorCount: formatErrorCount });
      const isShaderpackLanguage = kind === "languageFile" && modUnits.some((unit) => (
        unit.sourceKind === "shaderpack"
        && details.includedNamespaces.some((item) => item.modUnitId === unit.id && item.namespace === selectedNamespace)
      ));
      setExportCompletion({
        kind,
        outputPath: outputPaths[0] ?? (kind === "resourcePack" ? "" : defaultExportDirectory),
        translatedCount,
        missingCount,
        namespace: selectedNamespace,
        sourceKind: isShaderpackLanguage ? "shaderpack" : undefined,
        autoOpenOutputFolder: autoOpenOutputFolder && outputPaths.length > 0,
      });
      await goTo("exportComplete");
      if (!isCurrentProject(startingProjectId)) return;
    } catch (cause: unknown) {
      if (!isProjectChangedError(cause) && isCurrentProject(startingProjectId)) {
        setError(errorMessage(cause));
      }
    } finally {
      if (isCurrentProject(startingProjectId)) setBusy(false);
    }
  };

  const changeInPlace = (checked: boolean) => {
    if (checked && !window.confirm(
      "mods フォルダの jar を直接書き換えます。元の jar はプロジェクト内にバックアップし、あとで元に戻せます。ランチャーによっては mod の更新や整合性チェックで元に戻ることがあります。Minecraft を終了してから実行してください。",
    )) return;
    setReplaceInPlace(checked);
  };

  const displayIssues = useMemo<ExportIssueDto[]>(
    () => selectedIds.length > 0 ? validation?.issues ?? [] : [],
    [selectedIds.length, validation?.issues],
  );
  const missingRows = useMemo(() => {
    const grouped = new Map<string, { modName: string; count: number }>();
    for (const issue of displayIssues) {
      if (issue.code !== "untranslatedEntries") continue;
      const count = issue.count ?? (Number.parseInt(issue.message, 10) || 0);
      if (count <= 0) continue;
      const id = issue.modUnitId === null ? `name:${issue.modName ?? "unknown"}` : `unit:${issue.modUnitId}`;
      const group = grouped.get(id) ?? { modName: issue.modName ?? `mod ${issue.modUnitId ?? ""}`.trim(), count: 0 };
      group.count += count;
      grouped.set(id, group);
    }
    return [...grouped].map(([id, group]) => ({ kind: "missing" as const, id, ...group }));
  }, [displayIssues]);
  const missingTotal = missingRows.reduce((total, row) => total + row.count, 0);
  const placeholderRows = useMemo(() => {
    const grouped = new Map<string, { modName: string; issues: ExportIssueDto[] }>();
    for (const issue of displayIssues) {
      if (issue.code !== "placeholderWarning" && issue.code !== "placeholderError") continue;
      const id = issue.modUnitId === null ? `name:${issue.modName ?? "unknown"}` : `unit:${issue.modUnitId}`;
      const group = grouped.get(id) ?? { modName: issue.modName ?? `mod ${issue.modUnitId ?? ""}`.trim(), issues: [] };
      group.issues.push(issue);
      grouped.set(id, group);
    }
    return [...grouped].flatMap(([id, group]) => [
      { kind: "mod" as const, id: `group:${id}`, modName: group.modName },
      ...group.issues.map((issue, index) => ({
        kind: "placeholder" as const,
        id: `${id}:${issue.namespace ?? ""}:${issue.key ?? index}`,
        severity: issue.severity,
        namespace: issue.namespace ?? "",
        key: issue.key ?? issue.message,
        label: issue.displayLabel ?? `${issue.namespace ?? ""}:${issue.key ?? issue.message}`,
        sourceText: issue.sourceText ?? "",
        targetText: issue.targetText ?? "",
      })),
    ]);
  }, [displayIssues]);
  const placeholderCount = placeholderRows.reduce(
    (count, row) => count + Number(row.kind === "placeholder"),
    0,
  );
  const placeholderErrorCount = placeholderRows.reduce(
    (count, row) => count + Number(row.kind === "placeholder" && row.severity === "error"),
    0,
  );
  const placeholderWarningCount = placeholderCount - placeholderErrorCount;
  // 「このまま書き出す」は、警告を出した時点と同じ書式エラーの件数に対してだけ許可する。
  // 訳を直して件数が変わったり、書式エラー以外のエラーが出たりしたら、もう一度「書き出す」から確認し直させる。
  const canForceExport = forcePermit !== null
    && forcePermit.target === exportTarget
    && forcePermit.count === placeholderErrorCount
    && !displayIssues.some((issue) =>
      issue.severity === "error" && issue.code !== "placeholderError" && issue.code !== "modUnavailable",
    );
  const otherIssues = displayIssues.filter((issue) => (
    issue.code !== "untranslatedEntries"
      && issue.code !== "placeholderWarning"
      && issue.code !== "placeholderError"
  ));
  const blockingIssues = otherIssues.filter((issue) => issue.severity === "error");
  const warningIssues = otherIssues.filter((issue) => issue.severity === "warning");
  const issueLabel = (issue: ExportIssueDto) => issue.modName ? `${issue.modName}: ${issue.message}` : issue.message;

  if (!details || !projectId || loadedProjectId !== projectId) {
    return (
      <section className="focus-page export-screen">
        <output aria-live="polite">プロジェクトを読み込んでいます…</output>
        {error && <p className="status-message" role="alert">{error}</p>}
      </section>
    );
  }

  const activeReplacements = overview?.activeReplacements ?? [];

  return (
    <section className="focus-page export-screen" aria-labelledby="export-title">
      <h1 className="page-title" id="export-title">書き出し</h1>
      <div className="export-screen__counts">
        <p><strong>{details.counts.translated.toLocaleString("ja-JP")} / {details.counts.total.toLocaleString("ja-JP")}</strong> 翻訳済み</p>
      </div>

      <RadioGroup
        className="export-options"
        optionClassName="export-option"
        legend="書き出し形式"
        name="export-kind"
        value={kind}
        onChange={(value) => {
          const nextKind = value as ExportKind;
          setKind(nextKind);
          if (nextKind === "shaderpackZip") setDestination("folder");
        }}
        options={EXPORT_OPTIONS
          .filter((option) => option.value !== "modifiedJar" || hasJarUnit)
          .filter((option) => option.value !== "resourcePack" || hasJarUnit || hasModpackUnit)
          .filter((option) => option.value !== "modpackFiles" || hasModpackUnit)
          .filter((option) => option.value !== "shaderpackZip" || hasShaderpackUnit)
          .map((option) => ({
          ...option,
          description: option.value === "languageFile"
            ? `${languageFileNameForNamespace(details, selectedNamespace)}を書き出す`
            : option.value === "modifiedJar"
              ? replaceInPlace ? "mods フォルダのJARを直接上書きする" : "元のJARを変更せずコピーを書き出す"
              : option.value === "shaderpackZip"
                ? "元のシェーダーパックを残して別名 ZIP を書き出す"
              : option.description,
          id: `export-option-${option.value}`,
          testId: option.value === "languageFile" ? "export-language-file" : undefined,
        }))}
      />

      {selectableUnits.length > 1 && (
        <ModUnitSelector
          units={selectableUnits}
          selectedIds={selectedIds}
          onChange={(ids) => {
            setSelectedIds(ids);
            if (!overview?.instancePath || !overview.units.some((unit) => ids.includes(unit.id) && unit.originalExists)) {
              setReplaceInPlace(false);
            }
          }}
        />
      )}

      {kind === "resourcePack" && (
        <>
          <fieldset className="export-destination">
            <legend>保存先</legend>
            {overview?.instancePath && (
              <label>
                <input
                  type="radio"
                  name="resource-pack-destination"
                  value="instance"
                  checked={destination === "instance"}
                  onChange={() => setDestination("instance")}
                />
                インスタンスの resourcepacks に配置
              </label>
            )}
            <label>
              <input
                type="radio"
                name="resource-pack-destination"
                value="folder"
                checked={destination === "folder"}
                onChange={() => setDestination("folder")}
              />
              保存先を選ぶ
            </label>
          </fieldset>
          <div className="export-screen__field">
            <span>Minecraft バージョン</span>
            <Select
              aria-label="Minecraft バージョン"
              value={minecraftVersion}
              onChange={(version) => {
                setMinecraftVersion(version);
                setManualMinecraftVersion("");
              }}
              onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
              options={[
                { value: "", label: "選択" },
                ...versionOptions.map((version) => ({ value: version, label: version })),
              ]}
            />
          </div>
          {minecraftVersionEvidence(details.minecraftVersionDetection) && (
            <small className="export-screen__hint">
              {minecraftVersionEvidence(details.minecraftVersionDetection)}
            </small>
          )}
          <Disclosure className="export-screen__advanced" title="詳細設定">
            <label className="export-screen__field">
              表にない Minecraft バージョン
              <input
                value={manualMinecraftVersion}
                onChange={(event) => setManualMinecraftVersion(event.currentTarget.value)}
                onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
                placeholder="26.4"
              />
            </label>
            <div className="export-screen__field">
              <span>パック形式（大）</span>
              <NumberInput
                min="0"
                step="1"
                value={manualFormatMajor}
                onChange={(event) => setManualFormatMajor(event.currentTarget.value)}
                onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
                aria-label="パック形式（大）"
              />
            </div>
            <div className="export-screen__field">
              <span>パック形式（小）</span>
              <NumberInput
                min="0"
                step="1"
                value={manualFormatMinor}
                onChange={(event) => setManualFormatMinor(event.currentTarget.value)}
                onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
                aria-label="パック形式（小）"
              />
            </div>
          </Disclosure>
        </>
      )}

      {kind === "resourcePack" && selectedFtbQuestEntries > 0 && (
        <p className="export-screen__hint">
          FTB Quests のクエストはリソースパックに含まれません。「Modpack ファイル」から書き出してください。
        </p>
      )}

      {(kind === "resourcePack" || kind === "modifiedJar")
        && modUnits.some((unit) => selectedIdSet.has(unit.id) && unit.sourceKind === "shaderpack") && (
        <p className="export-screen__hint">
          シェーダーパックは{kind === "resourcePack" ? "リソースパック" : "Mod JAR"}に含まれません。「日本語化したシェーダーパック」から書き出してください。
        </p>
      )}

      {kind === "modpackFiles" && (
        <fieldset className="export-destination">
          <legend>保存先</legend>
          <label>
            <input
              type="radio"
              name="modpack-files-destination"
              value="folder"
              checked={destination === "folder"}
              onChange={() => setDestination("folder")}
            />
            ZIP に書き出す
          </label>
          {overview?.instancePath && (
            <label>
              <input
                type="radio"
                name="modpack-files-destination"
                value="instance"
                checked={destination === "instance"}
                onChange={() => setDestination("instance")}
              />
              インスタンスに直接配置
            </label>
          )}
        </fieldset>
      )}

      {kind === "shaderpackZip" && (
        <fieldset className="export-destination">
          <legend>保存先</legend>
          <label>
            <input
              type="radio"
              name="shaderpack-destination"
              value="folder"
              checked={destination === "folder"}
              onChange={() => setDestination("folder")}
            />
            ZIP に書き出す
          </label>
          {overview?.instancePath && (
            <label>
              <input
                type="radio"
                name="shaderpack-destination"
                value="instance"
                checked={destination === "instance"}
                onChange={() => setDestination("instance")}
              />
              インスタンスの shaderpacks に配置
            </label>
          )}
        </fieldset>
      )}

      {kind === "languageFile" && includedNamespaces.length > 1 && (
        <div className="export-screen__field">
          <span>名前空間</span>
          <Select
            aria-label="名前空間"
            value={selectedNamespace}
            onChange={setNamespace}
            options={includedNamespaces.map((item) => ({ value: item, label: item }))}
          />
        </div>
      )}

      {kind === "modifiedJar" && eligibleForInPlace && (
        <label className="export-screen__advanced-check">
          <input type="checkbox" checked={replaceInPlace} onChange={(event) => changeInPlace(event.currentTarget.checked)} />
          mods フォルダの JAR を直接上書きする
        </label>
      )}

      <section className="export-validation" aria-label="書き出しの確認" aria-live="polite">
        {validationError && <p className="status-message" role="alert">{validationError}</p>}
        {!!blockingIssues.length && (
          <ul ref={blockingIssuesRef} tabIndex={-1} className="export-issues" aria-label="書き出しできない理由">
            {blockingIssues.map((issue, index) => (
              <li className="export-issue export-issue--error" key={`${issue.modUnitId ?? "project"}-${issue.code}-${index}`}>
                {issueLabel(issue)}
              </li>
            ))}
          </ul>
        )}
        {missingTotal > 0 && (
          <details className="export-validation__details" onToggle={(event) => setMissingDetailsOpen(event.currentTarget.open)}>
            <summary>未翻訳 合計 {missingTotal.toLocaleString("ja-JP")} 件</summary>
            {missingDetailsOpen && <VirtualValidationList rows={missingRows} label="modごとの未翻訳件数" />}
          </details>
        )}
        {placeholderCount > 0 && (
          <details ref={placeholderDetailsRef} tabIndex={-1} className="export-validation__details" onToggle={(event) => setPlaceholderDetailsOpen(event.currentTarget.open)}>
            <summary className={placeholderErrorCount > 0 ? "export-issue--error" : undefined}>
              書式エラー {placeholderErrorCount.toLocaleString("ja-JP")} 件・注意 {placeholderWarningCount.toLocaleString("ja-JP")} 件
            </summary>
            {placeholderDetailsOpen && (
              <VirtualValidationList rows={placeholderRows} label="書式の確認事項" />
            )}
          </details>
        )}
        {!!warningIssues.length && (
          <ul className="export-issues" aria-label="その他の確認事項">
            {warningIssues.map((issue, index) => (
              <li className="export-issue" key={`${issue.modUnitId ?? "project"}-${issue.code}-${index}`}>
                {issueLabel(issue)}
              </li>
            ))}
          </ul>
        )}
      </section>
      {error && <p className="status-message" role="alert">{error}</p>}
      {canForceExport && (
        <SecondaryButton disabled={busy} onClick={() => void beginExport(forcePermit?.count ?? null)}>このまま書き出す</SecondaryButton>
      )}
      <footer className="export-screen__actions">
        <SecondaryButton disabled={busy} onClick={() => void goTo("editor")}>戻る</SecondaryButton>
        <PrimaryButton
          data-testid="export-start"
          disabled={busy || loadedProjectId !== projectId || selectedIds.length === 0
            || (kind === "languageFile" && !selectedNamespace)
            || (kind === "modifiedJar" && selectedJarIds.length === 0)
            || (kind === "shaderpackZip" && selectedShaderpackIds.length === 0)}
          onClick={() => void beginExport()}
        >
          {busy ? "書き出し中…" : "書き出す"}
        </PrimaryButton>
      </footer>

      {activeReplacements.length > 0 && (
        <section className="export-replacements" aria-labelledby="export-replacements-title">
          <h2 id="export-replacements-title">上書きしたJAR</h2>
          <ul>
            {activeReplacements.map((replacement) => {
              const unit = modUnits.find((candidate) => candidate.id === replacement.modUnitId);
              return (
                <li key={replacement.id}>
                  <div>
                    <strong>{unit ? unitTitle(unit) : `mod ${replacement.modUnitId}`}</strong>
                    <span className="selectable">{displayPath(replacement.targetPath)}</span>
                  </div>
                  <SecondaryButton
                    disabled={restoringId !== null}
                    onClick={() => void restoreJar(replacement.id)}
                  >
                    {restoringId === replacement.id ? "復元中…" : "元に戻す"}
                  </SecondaryButton>
                </li>
              );
            })}
          </ul>
          {restoreError && <p className="status-message" role="alert">{restoreError}</p>}
        </section>
      )}
      {!!overview?.activeFileDeployments?.length && (
        <section className="export-replacements" aria-labelledby="export-file-deployments-title">
          <h2 id="export-file-deployments-title">配置したファイル</h2>
          <ul>
            {overview.activeFileDeployments.map((deployment: FileDeploymentDto) => (
              <li key={deployment.id}>
                <div>
                  <span className="selectable">{displayPath(deployment.targetPath)}</span>
                </div>
                <SecondaryButton
                  disabled={restoringId !== null}
                  onClick={() => void restoreFile(deployment.id)}
                >
                  {restoringId === deployment.id ? "復元中…" : "元に戻す"}
                </SecondaryButton>
              </li>
            ))}
          </ul>
          {restoreError && <p className="status-message" role="alert">{restoreError}</p>}
        </section>
      )}
    </section>
  );
}
