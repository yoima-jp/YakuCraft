import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { join as joinPath } from "@tauri-apps/api/path";

import { PrimaryButton, SecondaryButton } from "../../components/button/Buttons";
import { Checkbox, Dialog, Disclosure, NumberInput, RadioGroup, Select } from "../../components/Controls";
import { useDialogConfirmation } from "../../components/useDialogConfirmation";
import { formatNumber, lowerText, t, useT, useUiLanguage } from "../../i18n";
import { targetLanguageName } from "../../i18n/minecraftLanguages";
import { exportMessages } from "../../i18n/messages/export";
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

const EXPORT_OPTIONS = [
  { value: "resourcePack", label: "export.format.resourcePack", description: "export.option.resourcePack" },
  { value: "modifiedJar", label: "export.format.modifiedJar", description: "export.option.modifiedJar" },
  { value: "languageFile", label: "export.format.languageFile", description: "export.option.languageFile" },
  { value: "modpackFiles", label: "export.format.modpackFiles", description: "export.option.modpackFiles" },
  { value: "shaderpackZip", label: "export.format.shaderpackZip", description: "export.option.shaderpackZip" },
] as const;

const DEFAULT_OUTPUT_NAMES = {
  modifiedJar: exportMessages.ja["export.fileName.modifiedJar"],
  modpackFiles: exportMessages.ja["export.fileName.modpackFiles"],
  languageFile: exportMessages.ja["export.fileName.languageFile"],
} as const;

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
  const targetLocale = details.targetLocale.toLowerCase().replaceAll("-", "_");
  const target = languages.find((language) => language.locale.toLowerCase().replaceAll("-", "_") === targetLocale);
  if (target) return target.archivePath.split(/[\\/]/).at(-1) ?? `${targetLocale}.json`;

  const source = languages.find((language) => language.locale.toLowerCase() === details.sourceLocale.toLowerCase());
  if (!source) return `${targetLocale}.json`;
  const sourceName = source.archivePath.split(/[\\/]/).at(-1) ?? "en_us.json";
  const dot = sourceName.lastIndexOf(".");
  if (dot < 0) return `${targetLocale}.json`;
  const stem = sourceName.slice(0, dot);
  const sourceRegion = stem.slice(stem.lastIndexOf("_") + 1);
  const upperRegion = sourceRegion.length > 0 && !/[a-z]/.test(sourceRegion);
  const [language, region] = targetLocale.split("_");
  const targetName = region && upperRegion ? `${language}_${region.toUpperCase()}` : targetLocale;
  return `${targetName}${sourceName.slice(dot)}`;
}

function errorMessage(error: unknown) {
  void logFrontendError(error, "export screen");
  if (error instanceof ExportCommandError || error instanceof AppCommandError) return error.message;
  return t("export.error.unexpected");
}

function isProjectChangedError(error: unknown) {
  return error instanceof ExportCommandError && error.code === "project_changed";
}

function minecraftVersionEvidence(detection: MinecraftVersionDetectionDto | null | undefined) {
  if (!detection) return null;
  if (detection.method === "metadata") {
    return detection.rounded
      ? t("export.versionEvidence.metadata.near", { value: detection.originalValue ?? "" })
      : t("export.versionEvidence.metadata.from", { value: detection.originalValue ?? "" });
  }
  if (detection.method === "packMcmeta") {
    return t(
      detection.rounded ? "export.versionEvidence.packMcmeta.near" : "export.versionEvidence.packMcmeta.from",
      { value: detection.originalValue ?? t("export.versionEvidence.valueFallback") },
    );
  }
  return t(detection.rounded
    ? "export.versionEvidence.resourceStructure.near"
    : "export.versionEvidence.resourceStructure.from");
}

function unitTitle(
  unit: { sourceKind?: "jar" | "modpack" | "shaderpack"; sourceFilename: string; name: string | null; version?: string | null },
  translate: ReturnType<typeof useT>,
) {
  if (unit.sourceKind === "modpack") return translate("export.unit.modpack");
  if (unit.sourceKind === "shaderpack") return translate("export.unit.shaderpack", { filename: unit.sourceFilename });
  const name = unit.name?.trim() || unit.sourceFilename;
  return unit.version?.trim() ? `${name} ${unit.version}` : name;
}

function exportOptionLabel(option: (typeof EXPORT_OPTIONS)[number], translate: ReturnType<typeof useT>, language: string) {
  if (option.value === "shaderpackZip") return translate(option.label, { language });
  return translate(option.label);
}

function exportOptionDescription(
  option: (typeof EXPORT_OPTIONS)[number],
  translate: ReturnType<typeof useT>,
  fileName: string,
  replaceInPlace: boolean,
) {
  if (option.value === "languageFile") {
    return translate("export.option.languageFileOutput", { fileName });
  }
  if (option.value === "modifiedJar") {
    return replaceInPlace
      ? translate("export.option.modifiedJarReplace")
      : translate("export.option.modifiedJarCopy");
  }
  if (option.value === "shaderpackZip") return translate("export.option.shaderpackOutput");
  return translate(option.description);
}

function isJarUnit(unit: { sourceKind?: "jar" | "modpack" | "shaderpack" }) {
  return unit.sourceKind === undefined || unit.sourceKind === "jar";
}

function selectableUnitsForKind(
  units: ReturnType<typeof useProjectStore.getState>["modUnits"],
  kind: ExportKind,
) {
  if (kind === "modifiedJar") return units.filter(isJarUnit);
  if (kind === "shaderpackZip") return units.filter((unit) => unit.sourceKind === "shaderpack");
  // 言語ファイルはシェーダーの lang 単体出力にも使うので、全ユニットを対象にする。
  // 全選択で selectedIds が一覧の内容に置き換わるため、ここで外すと後から再選択できなくなる。
  if (kind === "languageFile") return units;
  // シェーダーの lang はパック自体に書き込む方式なので、リソースパックなどの対象からは外す。
  // Modpack ユニットは KubeJS の翻訳をリソースパックにも含められるため、従来どおり残す。
  return units.filter((unit) => unit.sourceKind !== "shaderpack");
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
  const t = useT();
  const [query, setQuery] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const selected = useMemo(() => new Set(selectedIds), [selectedIds]);
  const filteredUnits = useMemo(() => {
    const normalized = lowerText(query.trim());
    if (!normalized) return units;
    return units.filter((unit) => lowerText(unitTitle(unit, t)).includes(normalized)
      || lowerText(unit.sourceFilename).includes(normalized));
  }, [query, t, units]);
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
      <legend>{t("export.unitSelector.legend")}</legend>
      <div className="export-unit-selector__toolbar">
        <input
          type="search"
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          aria-label={t("export.unitSelector.searchLabel")}
          placeholder={t("export.unitSelector.searchPlaceholder")}
          value={query}
          onChange={(event) => setQuery(event.currentTarget.value)}
        />
        <SecondaryButton onClick={() => onChange(units.map((unit) => unit.id))}>{t("export.unitSelector.selectAll")}</SecondaryButton>
        <SecondaryButton onClick={() => onChange([])}>{t("export.unitSelector.clearAll")}</SecondaryButton>
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
                <span>{unitTitle(unit, t)}</span>
              </label>
            );
          })}
        </div>
      </div>
      <small>{formatNumber(selectedIds.length)} / {formatNumber(units.length)}</small>
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

const AUTO_PROJECT_NAME_PATTERNS = [
  /^\d+ \u500b\u306e mod$/u,
  /^\d+ mod(s)?$/,
  /^\d+ \u4e2a\u6a21\u7ec4$/u,
  /^\d+ \u500b\u6a21\u7d44$/u,
  /^\ubaa8\ub4dc \d+\uac1c$/u,
];

function mergedPackName(projectName: string, instancePath: string | null) {
  const instanceName = instancePath?.replace(/[\\/]+$/, "").split(/[\\/]/).at(-1)?.trim();
  const name = instanceName || projectName.trim();
  // Rust 側の is_auto_project_name と同じ言語別書式を検出し、過去の名前も維持する。
  return !name || AUTO_PROJECT_NAME_PATTERNS.some((pattern) => pattern.test(name)) ? "YakuCraft" : name;
}

function mergeOutputFileName(
  units: ReturnType<typeof useProjectStore.getState>["modUnits"],
  selectedIds: number[],
  projectName: string,
  instancePath: string | null,
  targetLocale: string,
) {
  const normalizedTarget = targetLocale.toLowerCase().replaceAll("-", "_");
  const suffix = normalizedTarget === "ja_jp" ? "JP_EX" : `${normalizedTarget.toUpperCase()}_EX`;
  if (selectedIds.length > 1) return `${safeFileComponent(mergedPackName(projectName, instancePath))}-${suffix}.zip`;
  const unit = units.find((candidate) => candidate.id === selectedIds[0]);
  const sourceStem = (unit?.sourceFilename ?? "mod.jar").replace(/\.jar$/i, "");
  const name = safeFileComponent(unit?.name?.trim() || sourceStem);
  const version = unit?.version?.trim();
  return `${name}${version ? `-${safeFileComponent(version)}` : ""}-${suffix}.zip`;
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
  const t = useT();
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
                <span>{t(row.count === 1 ? "export.validation.missingCount_one" : "export.validation.missingCount", { count: formatNumber(row.count) })}</span>
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
                <span className={row.severity === "error" ? "export-validation__error-label" : undefined}>{row.severity === "error" ? t("export.validation.error") : t("export.validation.warning")}</span> · {row.label}
              </code>
              <p><strong>{t("export.validation.sourceText")}</strong><span className="selectable" title={row.sourceText}>{row.sourceText}</span></p>
              <p><strong>{t("export.validation.targetText")}</strong><span className="selectable" title={row.targetText}>{row.targetText}</span></p>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export function ExportScreen() {
  const t = useT();
  const uiLanguage = useUiLanguage();
  const { requestConfirmation, dialog: confirmationDialog } = useDialogConfirmation();
  const details = useProjectStore((state) => state.details);
  const targetLanguage = targetLanguageName(details?.targetLocale ?? "ja_jp");
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
  const [deployShaderpacksWithExport, setDeployShaderpacksWithExport] = useState(true);
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
    setDeployShaderpacksWithExport(true);
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
  const selectableUnits = selectableUnitsForKind(modUnits, kind);
  const selectedJarIds = modUnits
    .filter((unit) => selectedIdSet.has(unit.id) && isJarUnit(unit))
    .map((unit) => unit.id);
  const selectedShaderpackIds = modUnits
    .filter((unit) => selectedIdSet.has(unit.id) && unit.sourceKind === "shaderpack")
    .map((unit) => unit.id);
  const projectShaderpackIds = modUnits
    .filter((unit) => unit.sourceKind === "shaderpack")
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
  const outOfScopeInPlaceUnits = overview?.units.filter((unit) => (
    selectedIdSet.has(unit.id)
      && isJarUnit(unit)
      && unit.originalExists
      && !unit.inInstanceMods
  )) ?? [];
  const versionOptions = useMemo(() => [...new Set([
    ...(details?.supportedMinecraftVersions ?? []),
    ...modUnits.map((unit) => unit.minecraftVersion?.trim() ?? "").filter(Boolean),
  ])], [details?.supportedMinecraftVersions, modUnits]);
  const eligibleForInPlace = Boolean(overview?.instancePath)
    && overview?.units.some((unit) => selectedIdSet.has(unit.id)
      && isJarUnit(unit) && unit.originalExists && unit.inInstanceMods);
  const showShaderpackDeploymentOption = Boolean(overview?.instancePath && hasShaderpackUnit)
    && ((kind === "resourcePack" && destination === "instance")
      || (kind === "modifiedJar" && replaceInPlace && eligibleForInPlace));
  const deployShaderpacks = showShaderpackDeploymentOption && deployShaderpacksWithExport;

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
  }, [effectiveMinecraftVersion, isCurrentProject, kind, loadedProjectId, manualPackFormat, projectId, selectedIds, selectedNamespace, uiLanguage]);

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
        const confirmed = await requestConfirmation({
          title: t("export.confirm.restoreJar.title"),
          message: t("export.confirm.restoreJar.message"),
          confirmLabel: t("export.confirm.overwrite"),
          destructive: true,
        });
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

  const exportTarget = `${projectId ?? ""}:${kind}:${selectedIds.join(",")}:${deployShaderpacks}`;
  const confirmInstanceDeployment = () => requestConfirmation({
    title: t("export.confirm.place.title"),
    message: t("export.confirm.place.shaderpackMessage"),
    confirmLabel: t("export.confirm.place.action"),
    destructive: true,
  });
  const confirmModpackDeployment = () => requestConfirmation({
    title: t("export.confirm.place.title"),
    message: t("export.confirm.place.filesMessage"),
    confirmLabel: t("export.confirm.place.action"),
    destructive: true,
  });
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
      let result = await validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat, selectedIds);
      if (!result || !isCurrentProject(startingProjectId)) return;
      let shaderpackValidationIssues: ExportIssueDto[] = [];
      if (deployShaderpacks) {
        const shaderpackValidation = await validateSelectedProject(
          startingProjectId,
          "shaderpackZip",
          projectShaderpackIds,
        );
        if (!isCurrentProject(startingProjectId)) return;
        shaderpackValidationIssues = shaderpackValidation.issues;
        result = { ...result, issues: [...result.issues, ...shaderpackValidation.issues] };
        setValidation(result);
      }
      const blockingErrors = result.issues.filter((issue) =>
        issue.severity === "error" && issue.code !== "modUnavailable",
      ).filter((issue) => !shaderpackValidationIssues.includes(issue) || issue.code === "placeholderError");
      const formatErrorCount = result.issues.filter((issue) =>
        issue.severity === "error" && issue.code === "placeholderError",
      ).length;
      const formatConfirmationRequired = formatErrorCount > 0 && allowedCount !== formatErrorCount;
      if (kind === "modifiedJar" && formatConfirmationRequired) {
        setForcePermit({ target: exportTarget, count: formatErrorCount });
        return;
      }
      if (blockingErrors.length > 0 && kind !== "modifiedJar") {
        const onlyFormatErrors = formatErrorCount === blockingErrors.length;
        if (!onlyFormatErrors || formatConfirmationRequired) {
          if (onlyFormatErrors) {
            setForcePermit({ target: exportTarget, count: formatErrorCount });
          } else {
            setError(t("export.error.fixIssues"));
            focusIssue.current = "blocking";
          }
          return;
        }
      }

      if (deployShaderpacks || (kind === "shaderpackZip" && destination === "instance")) {
        if (!await confirmInstanceDeployment()) return;
        if (!isCurrentProject(startingProjectId)) return;
      }

      const defaultName = kind === "resourcePack"
        ? mergeOutputFileName(
          modUnits,
          selectedIds.filter((id) => details.includedNamespaces.some((item) => item.modUnitId === id)),
          details.project.displayName,
          overview?.instancePath ?? null,
          details.targetLocale,
        )
        : kind === "modifiedJar"
          ? DEFAULT_OUTPUT_NAMES.modifiedJar
          : kind === "modpackFiles"
            ? DEFAULT_OUTPUT_NAMES.modpackFiles
            : kind === "shaderpackZip"
              ? `${safeFileComponent(modUnits.find((unit) => selectedShaderpackIds.includes(unit.id))?.sourceFilename ?? "shaderpack")}-${details.targetLocale.toLowerCase() === "ja_jp" ? "ja" : details.targetLocale.toLowerCase().replaceAll("-", "_")}.zip`
              : DEFAULT_OUTPUT_NAMES.languageFile;
      const defaultPath = defaultExportDirectory
        ? await joinPath(defaultExportDirectory, defaultName)
        : defaultName;
      let outputPaths: string[] = [];
      let modResults: ModExportResultDto[] = [];
      let conflicts: Array<{ namespace: string; key: string; keptFrom: string; droppedFrom: string }> = [];
      let directlyInstalled = false;
      let inPlaceReplacement = false;
      let shaderpackInstalled = false;
      let shaderpackError: string | undefined;
      let translatedCount = kind === "modpackFiles" ? details.counts.translated : result.counts.translated;
      let missingCount = kind === "modpackFiles" ? details.counts.missing : result.counts.missing;

      if (kind === "modpackFiles") {
        let outputPath: string | null = null;
        if (destination === "folder") {
          outputPath = await saveFileDialog({ defaultPath: `${defaultPath}.zip`, filters: [{ name: "ZIP", extensions: ["zip"] }] });
          if (!isCurrentProject(startingProjectId)) return;
          if (!outputPath) return;
        } else {
          if (!await confirmModpackDeployment()) return;
          if (!isCurrentProject(startingProjectId)) return;
        }
        let outcome = await exportModpackFiles(
          startingProjectId, selectedIds, outputPath, destination === "instance", false,
        );
        if (!isCurrentProject(startingProjectId)) return;
        if (outcome.status === "overwriteRequired") {
          const confirmed = await requestConfirmation({
            title: t("export.confirm.overwriteFile.title"),
            message: t("export.confirm.overwriteFile.message", { path: displayPath(outcome.targetPath) }),
            confirmLabel: t("export.confirm.overwrite"),
            destructive: true,
          });
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
        let outcome = await exportShaderpacks(
          startingProjectId,
          selectedShaderpackIds,
          outputDirectory ?? null,
          destination === "instance",
          false,
        );
        if (!isCurrentProject(startingProjectId)) return;
        if (outcome.status === "overwriteRequired") {
          const confirmed = await requestConfirmation({
            title: t("export.confirm.overwriteShaderpack.title"),
            message: t("export.confirm.overwriteShaderpack.message", {
              paths: outcome.targetPaths
                .map((path) => t("export.pathListItem", { path: displayPath(path) }))
                .join("\n"),
            }),
            confirmLabel: t("export.confirm.overwrite"),
            destructive: true,
          });
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
          const confirmed = await requestConfirmation({
            title: t("export.confirm.overwriteResourcePack.title"),
            message: t("export.confirm.overwriteResourcePack.message"),
            confirmLabel: t("export.confirm.overwrite"),
            destructive: true,
          });
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
        if (deployShaderpacks) {
          modResults = modResults.filter((item) => !projectShaderpackIds.includes(item.modUnitId));
        }
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
            filters: [{ name: t("export.format.languageFile"), extensions: ["json", "lang"] }],
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

      if (deployShaderpacks) {
        let outcome: Awaited<ReturnType<typeof exportShaderpacks>> | null = null;
        try {
          outcome = await exportShaderpacks(
            startingProjectId,
            projectShaderpackIds,
            null,
            true,
            false,
          );
          if (!isCurrentProject(startingProjectId)) return;
          if (outcome.status === "overwriteRequired") {
            const confirmed = await requestConfirmation({
              title: t("export.confirm.overwriteShaderpack.title"),
              message: t("export.confirm.overwriteShaderpack.message", {
                paths: outcome.targetPaths
                  .map((path) => t("export.pathListItem", { path: displayPath(path) }))
                  .join("\n"),
              }),
              confirmLabel: t("export.confirm.overwrite"),
              destructive: true,
            });
            if (!isCurrentProject(startingProjectId)) return;
            if (confirmed) {
              outcome = await exportShaderpacks(
                startingProjectId,
                projectShaderpackIds,
                null,
                true,
                true,
              );
              if (!isCurrentProject(startingProjectId)) return;
            } else {
              outcome = null;
            }
          }
          if (outcome?.status === "completed") {
            outputPaths.push(...outcome.outputPaths);
            modResults.push(...outcome.modResults);
            shaderpackInstalled = outcome.outputPaths.length > 0;
            const failedShaderpacks = outcome.modResults.filter((item) => item.status !== "success");
            if (failedShaderpacks.length > 0) {
              shaderpackError = t("export.shaderpackFailure.list", {
                items: failedShaderpacks
                  .map((item) => t("export.shaderpackFailure.item", {
                    name: item.modName,
                    message: item.message ?? t("export.shaderpackFailure.itemUnavailable"),
                  }))
                  .join(" / "),
              });
            }
            if (outcome.directlyInstalled) {
              try {
                await refreshOverview(startingProjectId);
                if (!isCurrentProject(startingProjectId)) return;
              } catch (cause: unknown) {
                if (isProjectChangedError(cause)) return;
                shaderpackError ??= t("export.shaderpackFailure.refresh", { error: errorMessage(cause) });
              }
            }
          }
        } catch (cause: unknown) {
          if (isProjectChangedError(cause)) return;
          shaderpackError = t("export.shaderpackFailure.error", { error: errorMessage(cause) });
        }
      }

      setExportRunDetails({
        kind,
        directInstall: directlyInstalled,
        inPlaceReplacement,
        outputPaths,
        modResults,
        conflicts,
        placeholderErrorCount: formatErrorCount,
        shaderpackInstalled,
        shaderpackError,
      });
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

  const changeInPlace = async (checked: boolean) => {
    if (checked) {
      const startingProjectId = projectId;
      const confirmed = await requestConfirmation({
        title: t("export.confirm.replaceJar.title"),
        message: t("export.confirm.replaceJar.message"),
        confirmLabel: t("export.confirm.replaceJar.action"),
        destructive: true,
      });
      if (!confirmed || !startingProjectId || !isCurrentProject(startingProjectId)) return;
    }
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
      const fallbackName = issue.modUnitId === null
        ? t("export.unit.unknownMod")
        : t("export.unit.numberedMod", { id: formatNumber(issue.modUnitId) });
      const group = grouped.get(id) ?? { modName: issue.modName ?? fallbackName, count: 0 };
      group.count += count;
      grouped.set(id, group);
    }
    return [...grouped].map(([id, group]) => ({ kind: "missing" as const, id, ...group }));
  }, [displayIssues, t]);
  const missingTotal = missingRows.reduce((total, row) => total + row.count, 0);
  const placeholderRows = useMemo(() => {
    const grouped = new Map<string, { modName: string; issues: ExportIssueDto[] }>();
    for (const issue of displayIssues) {
      if (issue.code !== "placeholderWarning" && issue.code !== "placeholderError") continue;
      const id = issue.modUnitId === null ? `name:${issue.modName ?? "unknown"}` : `unit:${issue.modUnitId}`;
      const fallbackName = issue.modUnitId === null
        ? t("export.unit.unknownMod")
        : t("export.unit.numberedMod", { id: formatNumber(issue.modUnitId) });
      const group = grouped.get(id) ?? { modName: issue.modName ?? fallbackName, issues: [] };
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
  }, [displayIssues, t]);
  const placeholderCount = placeholderRows.reduce(
    (count, row) => count + Number(row.kind === "placeholder"),
    0,
  );
  const placeholderErrorCount = placeholderRows.reduce(
    (count, row) => count + Number(row.kind === "placeholder" && row.severity === "error"),
    0,
  );
  const placeholderWarningCount = placeholderCount - placeholderErrorCount;
  const otherIssues = displayIssues.filter((issue) => (
    issue.code !== "untranslatedEntries"
      && issue.code !== "placeholderWarning"
      && issue.code !== "placeholderError"
  ));
  const blockingIssues = otherIssues.filter((issue) => issue.severity === "error");
  const warningIssues = otherIssues.filter((issue) => issue.severity === "warning");
  const issueLabel = (issue: ExportIssueDto) => issue.modName
    ? t("export.validation.issueLabel", { name: issue.modName, message: issue.message })
    : issue.message;

  if (!details || !projectId || loadedProjectId !== projectId) {
    return (
      <section className="focus-page export-screen">
        <output aria-live="polite">{t("export.screen.loading")}</output>
        {error && <p className="status-message" role="alert">{error}</p>}
      </section>
    );
  }

  const activeReplacements = overview?.activeReplacements ?? [];

  return (
    <section className="focus-page export-screen" aria-labelledby="export-title">
      <h1 className="page-title" id="export-title">{t("export.screen.title")}</h1>
      <div className="export-screen__counts">
        <p>
          <strong>{formatNumber(details.counts.translated)} / {formatNumber(details.counts.total)}</strong>
          {t("export.screen.translatedSuffix")}
        </p>
      </div>

      <RadioGroup
        className="export-options"
        optionClassName="export-option"
        legend={t("export.option.legend")}
        name="export-kind"
        value={kind}
        onChange={(value) => {
          const nextKind = value as ExportKind;
          setKind(nextKind);
          const nextSelectableUnits = selectableUnitsForKind(modUnits, nextKind);
          const nextSelectableIds = nextSelectableUnits.map((unit) => unit.id);
          setSelectedIds((current) => {
            const retainedIds = current.filter((id) => nextSelectableIds.includes(id));
            if (nextSelectableUnits.length === 1) return nextSelectableIds;
            return retainedIds.length > 0 ? retainedIds : nextSelectableIds;
          });
          if (nextKind === "shaderpackZip") setDestination("folder");
        }}
        options={EXPORT_OPTIONS
          .filter((option) => option.value !== "modifiedJar" || hasJarUnit)
          .filter((option) => option.value !== "resourcePack" || hasJarUnit || hasModpackUnit)
          .filter((option) => option.value !== "modpackFiles" || hasModpackUnit)
          .filter((option) => option.value !== "shaderpackZip" || hasShaderpackUnit)
          .map((option) => ({
            value: option.value,
            label: exportOptionLabel(option, t, targetLanguage),
            description: exportOptionDescription(
              option,
              t,
              languageFileNameForNamespace(details, selectedNamespace),
              replaceInPlace,
            ),
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
            if (!overview?.instancePath || !overview.units.some((unit) => (
              ids.includes(unit.id) && unit.originalExists && unit.inInstanceMods
            ))) {
              setReplaceInPlace(false);
            }
          }}
        />
      )}

      {kind === "resourcePack" && (
        <>
          <fieldset className="export-destination">
            <legend>{t("export.destination.legend")}</legend>
            {overview?.instancePath && (
              <label>
                <input
                  type="radio"
                  name="resource-pack-destination"
                  value="instance"
                  checked={destination === "instance"}
                  onChange={() => setDestination("instance")}
                />
                {t("export.destination.instanceResourcePack")}
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
              {t("export.destination.choose")}
            </label>
          </fieldset>
          <div className="export-screen__field">
            <span>{t("export.field.minecraftVersion")}</span>
            <Select
              aria-label={t("export.field.minecraftVersion")}
              value={minecraftVersion}
              onChange={(version) => {
                setMinecraftVersion(version);
                setManualMinecraftVersion("");
              }}
              onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
              options={[
                { value: "", label: t("export.field.choose") },
                ...versionOptions.map((version) => ({ value: version, label: version })),
              ]}
            />
          </div>
          {minecraftVersionEvidence(details.minecraftVersionDetection) && (
            <small className="export-screen__hint">
              {minecraftVersionEvidence(details.minecraftVersionDetection)}
            </small>
          )}
          <Disclosure className="export-screen__advanced" title={t("export.field.advancedSettings")}>
            <label className="export-screen__field">
              {t("export.field.minecraftVersionNotListed")}
              <input
                value={manualMinecraftVersion}
                onChange={(event) => setManualMinecraftVersion(event.currentTarget.value)}
                onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
                placeholder="26.4"
              />
            </label>
            <div className="export-screen__field">
              <span>{t("export.field.packFormatMajor")}</span>
              <NumberInput
                min="0"
                step="1"
                value={manualFormatMajor}
                onChange={(event) => setManualFormatMajor(event.currentTarget.value)}
                onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
                aria-label={t("export.field.packFormatMajor")}
              />
            </div>
            <div className="export-screen__field">
              <span>{t("export.field.packFormatMinor")}</span>
              <NumberInput
                min="0"
                step="1"
                value={manualFormatMinor}
                onChange={(event) => setManualFormatMinor(event.currentTarget.value)}
                onBlur={() => void validate(kind, effectiveMinecraftVersion, selectedNamespace, manualPackFormat)}
                aria-label={t("export.field.packFormatMinor")}
              />
            </div>
          </Disclosure>
        </>
      )}

      {kind === "resourcePack" && selectedFtbQuestEntries > 0 && (
        <p className="export-screen__hint">
          {t("export.hint.ftbQuests")}
        </p>
      )}

      {(kind === "resourcePack" || kind === "modifiedJar")
        && selectedShaderpackIds.length > 0
        && !showShaderpackDeploymentOption && (
        <p className="export-screen__hint">
          {t("export.hint.shaderpackNotIncluded", {
            format: t(kind === "resourcePack" ? "export.format.resourcePack" : "export.format.modJar"),
            shaderpackOption: t("export.format.shaderpackZip", { language: targetLanguage }),
          })}
        </p>
      )}

      {showShaderpackDeploymentOption && (
        <Checkbox checked={deployShaderpacksWithExport} onChange={setDeployShaderpacksWithExport}>
          {t("export.checkbox.deployShaderpack")}
        </Checkbox>
      )}

      {kind === "modpackFiles" && (
        <fieldset className="export-destination">
          <legend>{t("export.destination.legend")}</legend>
          <label>
            <input
              type="radio"
              name="modpack-files-destination"
              value="folder"
              checked={destination === "folder"}
              onChange={() => setDestination("folder")}
            />
            {t("export.destination.zip")}
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
              {t("export.destination.instance")}
            </label>
          )}
        </fieldset>
      )}

      {kind === "shaderpackZip" && (
        <fieldset className="export-destination">
          <legend>{t("export.destination.legend")}</legend>
          <label>
            <input
              type="radio"
              name="shaderpack-destination"
              value="folder"
              checked={destination === "folder"}
              onChange={() => setDestination("folder")}
            />
            {t("export.destination.zip")}
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
              {t("export.destination.instanceShaderpack")}
            </label>
          )}
        </fieldset>
      )}

      {kind === "languageFile" && includedNamespaces.length > 1 && (
        <div className="export-screen__field">
          <span>{t("export.field.namespace")}</span>
          <Select
            aria-label={t("export.field.namespace")}
            value={selectedNamespace}
            onChange={setNamespace}
            options={includedNamespaces.map((item) => ({ value: item, label: item }))}
          />
        </div>
      )}

      {kind === "modifiedJar" && eligibleForInPlace && (
        <>
          <label className="export-screen__advanced-check">
            <input type="checkbox" checked={replaceInPlace} onChange={(event) => void changeInPlace(event.currentTarget.checked)} />
            {t("export.checkbox.replaceJar")}
          </label>
          {replaceInPlace && outOfScopeInPlaceUnits.length > 0 && (
            <p className="status-message" aria-live="polite">
              {t("export.error.outOfScopeReplacement", {
                units: outOfScopeInPlaceUnits.map((unit) => unitTitle(unit, t)).join(t("export.listSeparator")),
              })}
            </p>
          )}
        </>
      )}

      <section className="export-validation" aria-label={t("export.validation.section")} aria-live="polite">
        {validationError && <p className="status-message" role="alert">{validationError}</p>}
        {!!blockingIssues.length && (
          <ul ref={blockingIssuesRef} tabIndex={-1} className="export-issues" aria-label={t("export.validation.blockingList")}>
            {blockingIssues.map((issue, index) => (
              <li className="export-issue export-issue--error" key={`${issue.modUnitId ?? "project"}-${issue.code}-${index}`}>
                {issueLabel(issue)}
              </li>
            ))}
          </ul>
        )}
        {missingTotal > 0 && (
          <details className="export-validation__details" onToggle={(event) => setMissingDetailsOpen(event.currentTarget.open)}>
            <summary>{t(missingTotal === 1 ? "export.validation.missingSummary_one" : "export.validation.missingSummary", { count: formatNumber(missingTotal) })}</summary>
            {missingDetailsOpen && <VirtualValidationList rows={missingRows} label={t("export.validation.missingList")} />}
          </details>
        )}
        {placeholderCount > 0 && (
          <details ref={placeholderDetailsRef} tabIndex={-1} className="export-validation__details" onToggle={(event) => setPlaceholderDetailsOpen(event.currentTarget.open)}>
            <summary className={placeholderErrorCount > 0 ? "export-issue--error" : undefined}>
              {t("export.validation.placeholderSummary", {
                errors: formatNumber(placeholderErrorCount),
                warnings: formatNumber(placeholderWarningCount),
              })}
            </summary>
            {placeholderDetailsOpen && (
              <VirtualValidationList rows={placeholderRows} label={t("export.validation.placeholderList")} />
            )}
          </details>
        )}
        {!!warningIssues.length && (
          <ul className="export-issues" aria-label={t("export.validation.otherIssues")}>
            {warningIssues.map((issue, index) => (
              <li className="export-issue" key={`${issue.modUnitId ?? "project"}-${issue.code}-${index}`}>
                {issueLabel(issue)}
              </li>
            ))}
          </ul>
        )}
      </section>
      {error && <p className="status-message" role="alert">{error}</p>}
      {confirmationDialog}
      <Dialog open={forcePermit !== null && forcePermit.target === exportTarget} title={t("export.dialog.formatErrors.title")} onClose={() => {
        setForcePermit(null);
        if (placeholderDetailsRef.current) placeholderDetailsRef.current.open = true;
      }}>
        <p>{forcePermit && t(
          forcePermit.count === 1 ? "export.dialog.formatErrors.message_one" : "export.dialog.formatErrors.message",
          { count: formatNumber(forcePermit.count) },
        )}</p>
        <div className="dialog-confirmation__actions">
          <SecondaryButton onClick={() => {
            setForcePermit(null);
            if (placeholderDetailsRef.current) placeholderDetailsRef.current.open = true;
          }}>{t("export.action.back")}</SecondaryButton>
          <SecondaryButton disabled={busy} onClick={() => void beginExport(forcePermit?.count ?? null)}>{t("export.action.exportAnyway")}</SecondaryButton>
        </div>
      </Dialog>
      <footer className="export-screen__actions">
        <SecondaryButton disabled={busy} onClick={() => void goTo("editor")}>{t("export.action.back")}</SecondaryButton>
        <PrimaryButton
          data-testid="export-start"
          disabled={busy || loadedProjectId !== projectId || selectedIds.length === 0
            || (kind === "languageFile" && !selectedNamespace)
            || (kind === "modifiedJar" && selectedJarIds.length === 0)
            || (kind === "shaderpackZip" && selectedShaderpackIds.length === 0)}
          onClick={() => void beginExport()}
        >
          {busy ? t("export.action.exporting") : t("export.action.export")}
        </PrimaryButton>
      </footer>

      {activeReplacements.length > 0 && (
        <section className="export-replacements" aria-labelledby="export-replacements-title">
          <h2 id="export-replacements-title">{t("export.section.replacedJars")}</h2>
          <ul>
            {activeReplacements.map((replacement) => {
              const unit = modUnits.find((candidate) => candidate.id === replacement.modUnitId);
              return (
                <li key={replacement.id}>
                  <div>
                    <strong>{unit
                      ? unitTitle(unit, t)
                      : t("export.unit.numberedMod", { id: formatNumber(replacement.modUnitId) })}</strong>
                    <span className="selectable">{displayPath(replacement.targetPath)}</span>
                  </div>
                  <SecondaryButton
                    disabled={restoringId !== null}
                    onClick={() => void restoreJar(replacement.id)}
                  >
                    {restoringId === replacement.id ? t("export.action.restoring") : t("export.action.restore")}
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
          <h2 id="export-file-deployments-title">{t("export.section.deployedFiles")}</h2>
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
                  {restoringId === deployment.id ? t("export.action.restoring") : t("export.action.restore")}
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
