import { AppCommandError, entryFromProject, getAiSettings, listEntries, listModUnits, startAiJob } from "../../lib/tauri";
import { flushBeforeLeavingEditor } from "../../lib/navigation";
import type { RequestConfirmation } from "../../components/useDialogConfirmation";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useAppStore } from "../../stores/appStore";
import { useEditorStore } from "../../stores/editorStore";
import { useProjectStore } from "../../stores/projectStore";
import { useSettingsStore } from "../../stores/settingsStore";
import type { AiJobEventDto } from "../../types/ai";
import type { TranslationEntry } from "../../types/translation";
import type { EntryEdit } from "../../stores/editorStore";
import type { AiProviderId } from "../../types/ai";

const PROVIDER_LABELS: Record<AiProviderId, string> = {
  openai: "OpenAI",
  "openai-compatible": "OpenAI互換プロバイダー",
  deepl: "DeepL",
  chatgpt: "ChatGPT",
  anthropic: "Claude",
  gemini: "Gemini",
  openrouter: "OpenRouter",
  deepseek: "DeepSeek",
  groq: "Groq",
  ollama: "Ollama",
  "lm-studio": "LM Studio",
};

let unitScopedAiJobActive = false;
let aiJobStartReserved = false;

function reserveAiJobStart() {
  if (aiJobStartReserved || useAiJobStore.getState().activeJob) return false;
  aiJobStartReserved = true;
  return true;
}

function releaseAiJobStart() {
  aiJobStartReserved = false;
}

export function isUnitScopedAiJobActive() {
  return unitScopedAiJobActive;
}

export function translateSelectedWithAi(requestConfirmation: RequestConfirmation, fallbackEntryId?: number) {
  const state = useEditorStore.getState();
  const selectedIds = state.orderedIds.filter((id) => state.selectedIdSet.has(id));
  // AI翻訳の入口間で選択範囲の解決を共有し、Inspectorも複数選択時に同じ行集合を使う。
  const entryIds = selectedIds.length > 1 || fallbackEntryId === undefined
    ? selectedIds
    : [fallbackEntryId];
  return translateWithAi(entryIds, requestConfirmation);
}

export async function translateWithAi(entryIds: number[], requestConfirmation: RequestConfirmation) {
  if (!reserveAiJobStart()) return;

  try {
    const state = useEditorStore.getState();
    const ids = [...new Set(entryIds)].filter((id) => state.entries.has(id));
    const startingProjectId = state.loadedProjectId;
    if (
      ids.length === 0 ||
      !startingProjectId ||
      useProjectStore.getState().details?.project.id !== startingProjectId
    ) return;

    const settings = await getAiSettings();
    if (!settings.configured) {
      await useAppStore.getState().openSettings("ai");
      return;
    }

    const settingsStore = useSettingsStore.getState();
    if (!settingsStore.value<boolean>("ai.privacyNoticeAccepted")) {
      const providerLabel = PROVIDER_LABELS[settings.provider];
      const accepted = await requestConfirmation({
        title: "AI翻訳の送信内容を確認",
        message: `${providerLabel} に選んだ項目のキーと原文、Mod 名・namespace・言語情報、設定済みの追加プロンプトを送信します。JAR やテクスチャは送信しません。`,
        confirmLabel: "続ける",
      });
      if (!accepted) return;
      await settingsStore.save({ "ai.privacyNoticeAccepted": true });
    }

    // AI設定・同意確認の待機中に加えた編集もDBへ反映してから、最後の状態で確認する。
    if (!(await flushBeforeLeavingEditor())) return;
    const currentEditor = useEditorStore.getState();
    const project = useProjectStore.getState().details;
    if (currentEditor.loadedProjectId !== startingProjectId || project?.project.id !== startingProjectId) return;
    const selected = ids.flatMap((id) => {
      const entry = currentEditor.entries.get(id);
      return entry ? [entry] : [];
    });
    if (selected.length === 0) return;

    const overwriteCount = selected.filter((entry) => entry.targetText.length > 0).length;
    if (
      overwriteCount > 0 &&
      useSettingsStore.getState().value<boolean>("editor.confirmBatchOverwrite") &&
      !(await requestConfirmation({
        title: "既存の翻訳を上書きしますか？",
        message: `${overwriteCount}件の翻訳が置き換わります。`,
        confirmLabel: "上書き",
      }))
    ) return;

    const selectedIds = selected.map((entry) => entry.id);
    const expectedEntries = selected.map(({ id, targetText, origin }) => ({ id, targetText, origin }));
    // Project解析済みのMod情報を使い、AIジョブごとのJAR再解析を避ける。
    const modNamesByUnitAndNamespace = modNamesForProject(project);
    if (!useAiJobStore.getState().begin(settings.provider, selectedIds, selectedIds.length, startingProjectId)) return;
    unitScopedAiJobActive = true;
    const jobId = await startAiJob(
      selectedIds,
      settings.provider,
      expectedEntries,
      modNamesByUnitAndNamespace,
      startingProjectId,
      (event) => {
        useAiJobStore.getState().receive(event);
        if (event.kind === "completed" || event.kind === "cancelled" || event.kind === "failed") {
          unitScopedAiJobActive = false;
        }
      },
    );
    useAiJobStore.getState().attachJobId(jobId);
  } catch (error: unknown) {
    unitScopedAiJobActive = false;
    if (error instanceof AppCommandError && error.appError.code === "ai_not_configured") {
      await useAppStore.getState().openSettings("ai");
      return;
    }
    const message = error instanceof Error ? error.message : "AI翻訳を実行できませんでした。";
    useAiJobStore.getState().failToStart(message);
  } finally {
    releaseAiJobStart();
  }
}

export async function translateWholeProjectWithAi(requestConfirmation: RequestConfirmation, modUnitId?: number) {
  if (!reserveAiJobStart()) return;

  try {
    const startingProjectId = useProjectStore.getState().details?.project.id;
    if (!startingProjectId || useEditorStore.getState().loadedProjectId !== startingProjectId) return;
    const settings = await getAiSettings();
    if (!settings.configured) {
      await useAppStore.getState().openSettings("ai");
      return;
    }
    const scope = modUnitId === undefined ? "project" : "mod";
    if (!(await confirmAiPrivacy(settings.provider, requestConfirmation, scope))) return;
    if (!(await flushBeforeLeavingEditor())) return;

    const project = useProjectStore.getState().details;
    if (!project || project.project.id !== startingProjectId) return;
    const allEntries = await listEntries(modUnitId ?? null);
    const missing = allEntries.filter((entry) => entry.targetText.length === 0);
    if (missing.length === 0) return;
    if (!(await requestConfirmation({
      title: modUnitId === undefined ? "プロジェクト全体をAI翻訳しますか？" : "このmodの未翻訳をAI翻訳しますか？",
      message: `${missing.length.toLocaleString("ja-JP")}件の未翻訳を翻訳します。キーと原文、Mod名・namespace・言語情報をAIプロバイダーに送信します。`,
      confirmLabel: "翻訳",
    }))) return;

    if (!(await flushBeforeLeavingEditor())) return;
    const currentEditor = useEditorStore.getState();
    if (
      currentEditor.loadedProjectId !== startingProjectId ||
      useProjectStore.getState().details?.project.id !== startingProjectId
    ) return;

    // 確認中に保存済みデータが変わった場合に備え、送信直前のDB内容で件数とbaselineを確定する。
    const refreshedEntries = await listEntries(modUnitId ?? null);
    const batch = refreshedEntries.filter((entry) => entry.targetText.length === 0);
    if (batch.length === 0) return;
    if (batch.length !== missing.length && !(await requestConfirmation({
      title: "未翻訳の件数が変わりました",
      message: `現在の未翻訳は${batch.length.toLocaleString("ja-JP")}件です。`,
      confirmLabel: "続ける",
    }))) return;

    const entryIds = batch.map((entry) => entry.id);
    const expectedEntries = batch.map(({ id, targetText, origin }) => ({ id, targetText, origin }));
    const modNamesByUnitAndNamespace = modNamesForProject(project);
    const initialEntries = new Map(batch.map((entry) => [entry.id, entry]));
    const changesByUnit = new Map<number, EntryEdit[]>();
    const historyBoundariesByUnit = new Map<number, EntryEdit[][]>();
    const historyEditor = useEditorStore.getState();
    for (const entry of batch) {
      const modUnitId = entry.modUnitId;
      if (modUnitId === undefined || historyBoundariesByUnit.has(modUnitId)) continue;
      const cached = historyEditor.unitStates.get(modUnitId);
      const boundary = historyEditor.activeModUnitId === modUnitId
        ? historyEditor.undoStack
        : cached?.projectId === startingProjectId
          ? cached.undoStack
          : [];
      historyBoundariesByUnit.set(modUnitId, boundary);
    }
    if (useAiJobStore.getState().activeJob) return;
    useAiJobStore.setState({
      activeJob: { id: null, provider: settings.provider, completed: 0, total: entryIds.length },
      baseline: new Map(),
      pendingEdits: [],
      undoBoundary: [],
      notice: null,
      projectId: startingProjectId,
      jobProjectId: startingProjectId,
      jobProjectInvalidated: false,
      warnings: useAiJobStore.getState().projectId === startingProjectId
        ? useAiJobStore.getState().warnings
        : new Map(),
    });

    const finish = async (event: Extract<AiJobEventDto, { kind: "completed" | "cancelled" | "failed" }>) => {
      const editor = useEditorStore.getState();
      const projectStillOpen =
        editor.loadedProjectId === startingProjectId &&
        useProjectStore.getState().details?.project.id === startingProjectId;
      let refreshFailed = false;
      if (projectStillOpen) {
        for (const [modUnitId, changes] of changesByUnit) {
          editor.finishExternalModUnitAiUndoGroup(
            modUnitId,
            changes,
            historyBoundariesByUnit.get(modUnitId) ?? [],
            startingProjectId,
          );
        }
        const openedIds = [
          ...(editor.activeModUnitId === null ? [] : [editor.activeModUnitId]),
          ...editor.unitStates.keys(),
        ];
        const refreshResults = await Promise.allSettled(openedIds.map(async (modUnitId) => {
          const refreshEditor = useEditorStore.getState();
          const baseline = refreshEditor.activeModUnitId === modUnitId
            ? refreshEditor.entries
            : refreshEditor.unitStates.get(modUnitId)?.entries;
          const entries = await listEntries(modUnitId);
          if (
            useEditorStore.getState().loadedProjectId !== startingProjectId ||
            useProjectStore.getState().details?.project.id !== startingProjectId
          ) return;
          useEditorStore.getState().refreshCachedModUnitEntries(modUnitId, entries, baseline);
        }));
        refreshFailed = refreshResults.some((result) => result.status === "rejected");
        try {
          const latestUnits = await listModUnits();
          if (useProjectStore.getState().details?.project.id === startingProjectId) {
            useProjectStore.getState().setModUnits(latestUnits);
          }
        } catch {
          refreshFailed = true;
        }
      }

      const message = refreshFailed
        ? "翻訳結果を読み込めませんでした。画面を開き直してください。"
        : event.kind === "cancelled"
        ? "AI翻訳をキャンセルしました。"
        : event.kind === "failed"
          ? event.error.userMessage
          : null;
      useAiJobStore.setState({
        activeJob: null,
        jobProjectId: null,
        jobProjectInvalidated: false,
        baseline: new Map(),
        pendingEdits: [],
        undoBoundary: [],
        notice: message,
      });
    };

    const jobId = await startAiJob(
      entryIds,
      settings.provider,
      expectedEntries,
      modNamesByUnitAndNamespace,
      startingProjectId,
      (event) => {
        const activeJob = useAiJobStore.getState().activeJob;
        if (!activeJob) return;
        if (event.kind === "completed" || event.kind === "cancelled" || event.kind === "failed") {
          void finish(event);
          return;
        }
        if (
          useEditorStore.getState().loadedProjectId !== startingProjectId ||
          useProjectStore.getState().details?.project.id !== startingProjectId
        ) return;
        if (event.kind === "progress") {
          useAiJobStore.setState({ activeJob: { ...activeJob, id: event.jobId, completed: event.completed, total: event.total } });
          return;
        }
        if (event.kind === "batchResult") {
          const editor = useEditorStore.getState();
          if (
            editor.loadedProjectId !== startingProjectId ||
            useProjectStore.getState().details?.project.id !== startingProjectId
          ) return;
          const updatesByUnit = new Map<number, TranslationEntry[]>();
          for (const dto of event.entries) {
            const incoming = entryFromProject(dto);
            const modUnitId = incoming.modUnitId;
            if (modUnitId === undefined) continue;
            const initial = initialEntries.get(incoming.id);
            const local = cachedEntryForUnit(editor, modUnitId, incoming.id, startingProjectId);
            if (!initial || (local && !sameBaseline(local, initial))) continue;
            // unopened mod もDB baselineからUndo情報を作り、後から開いたとき履歴へ復元する。
            const before = local ?? initial;
            const change: EntryEdit = {
              entryId: incoming.id,
              before: before.targetText,
              after: incoming.targetText,
              beforeOrigin: before.origin,
              afterOrigin: incoming.origin,
              beforeFlags: [...before.flags],
              afterFlags: [...incoming.flags],
              timestamp: Date.now(),
            };
            if (change.before !== change.after || change.beforeOrigin !== change.afterOrigin || !flagsEqual(change.beforeFlags, change.afterFlags)) {
              const changes = changesByUnit.get(modUnitId) ?? [];
              changes.push(change);
              changesByUnit.set(modUnitId, changes);
              if (local) {
                const updates = updatesByUnit.get(modUnitId) ?? [];
                updates.push(incoming);
                updatesByUnit.set(modUnitId, updates);
              }
            }
          }
          for (const [modUnitId, entries] of updatesByUnit) {
            // aiJobStoreのbaselineは表示中の1unit分なので、全体翻訳では開いているsnapshotだけを直接更新する。
            useEditorStore.getState().mergeExternalModUnitEntries(modUnitId, entries);
          }
          const warnings = new Map(useAiJobStore.getState().warnings);
          for (const entry of event.entries) warnings.delete(entry.id);
          for (const rejection of event.rejected) warnings.set(rejection.id, rejection.reasons);
          if (event.entries.length > 0 || event.rejected.length > 0) useAiJobStore.setState({ warnings });
          useAiJobStore.setState({ activeJob: { ...activeJob, id: event.jobId } });
          return;
        }
      },
    );
    const activeJob = useAiJobStore.getState().activeJob;
    if (activeJob) useAiJobStore.setState({ activeJob: { ...activeJob, id: jobId } });
  } catch (error: unknown) {
    const message = error instanceof Error ? error.message : "AI翻訳を実行できませんでした。";
    useAiJobStore.getState().failToStart(message);
  } finally {
    releaseAiJobStart();
  }
}

async function confirmAiPrivacy(
  provider: AiProviderId,
  requestConfirmation: RequestConfirmation,
  scope: "mod" | "project",
) {
  const settingsStore = useSettingsStore.getState();
  if (settingsStore.value<boolean>("ai.privacyNoticeAccepted")) return true;
  const selection = scope === "project" ? "プロジェクト内の未翻訳項目" : "このmodの未翻訳項目";
  const accepted = await requestConfirmation({
    title: "AI翻訳の送信内容を確認",
    message: `${PROVIDER_LABELS[provider]} に${selection}のキーと原文、Mod 名・namespace・言語情報、設定済みの追加プロンプトを送信します。JAR やテクスチャは送信しません。`,
    confirmLabel: "続ける",
  });
  if (!accepted) return false;
  await settingsStore.save({ "ai.privacyNoticeAccepted": true });
  return true;
}

function modNamesForProject(project: NonNullable<ReturnType<typeof useProjectStore.getState>["details"]>) {
  const modNamesByUnitAndNamespace: Record<number, Record<string, string>> = {};
  for (const namespace of project.namespaces) {
    const mod = project.mods.find((candidate) => candidate.modUnitId === namespace.modUnitId);
    const modName = namespace.containerPath
      ? project.nestedArchives.find((archive) =>
          archive.modUnitId === namespace.modUnitId && archive.archivePath === namespace.containerPath,
        )?.name
      : project.mods.find((candidate) =>
          candidate.modUnitId === namespace.modUnitId && candidate.modId === namespace.namespace,
        )?.name ?? mod?.name;
    (modNamesByUnitAndNamespace[namespace.modUnitId] ??= {})[namespace.namespace] = modName || namespace.namespace;
  }
  return modNamesByUnitAndNamespace;
}

function cachedEntryForUnit(
  editor: ReturnType<typeof useEditorStore.getState>,
  modUnitId: number,
  entryId: number,
  projectId: string,
) {
  if (editor.activeModUnitId === modUnitId) return editor.entries.get(entryId);
  const snapshot = editor.unitStates.get(modUnitId);
  return snapshot?.projectId === projectId ? snapshot.entries.get(entryId) : undefined;
}

function sameBaseline(current: TranslationEntry, baseline: TranslationEntry) {
  return current.targetText === baseline.targetText && current.origin === baseline.origin && flagsEqual(current.flags, baseline.flags);
}

function flagsEqual(left: string[], right: string[]) {
  return left.length === right.length && left.every((flag, index) => flag === right[index]);
}
