import { useCallback, useEffect, useRef, useState } from "react";
import type { KeyboardEvent, MouseEvent } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import { DestructiveButton, PrimaryButton, SecondaryButton, TertiaryButton } from "../../components/button/Buttons";
import { Dialog, Tooltip } from "../../components/Controls";
import { ContextMenu, ContextMenuItem, ContextMenuSeparator, shouldPreserveNativeContextMenu, useContextMenuTrigger } from "../../components/ContextMenu";
import { DropArea } from "../../components/layout/DropArea";
import { AppCommandError, cancelAnalysis, createProjectFromJars, deleteProject, getProjectSummary, listEntries, listModUnits, listRecentProjects, openProject, renameProject } from "../../lib/tauri";
import { openFileDialog } from "../../lib/dialogs";
import { isImeCompositionKeyDown } from "../../lib/keyboard";
import { flushBeforeLeavingEditor } from "../../lib/navigation";
import type { AnalysisChannelEvent } from "../../types/commands";
import { useAppStore } from "../../stores/appStore";
import { useEditorStore } from "../../stores/editorStore";
import { useProjectStore } from "../../stores/projectStore";
import { useSettingsStore } from "../../stores/settingsStore";
import { useScanStore } from "../scan/scanStore";
import { UpdaterNotice } from "../updater/UpdaterNotice";
import "./HomeScreen.css";

// 解析中はこの画面がアンマウントされるため、世代番号は画面の再生成をまたいで保持する。
let currentAnalysisGeneration = 0;

function fileNameFromPath(path: string): string {
  const normalizedPath = path.replaceAll("\\", "/");
  return normalizedPath.slice(normalizedPath.lastIndexOf("/") + 1);
}

function isJarFilePath(path: string) {
  return fileNameFromPath(path).toLowerCase().endsWith(".jar");
}

function errorMessage(error: unknown) {
  if (error instanceof AppCommandError) return error.message;
  return "処理に失敗しました。もう一度お試しください。";
}

export function HomeScreen() {
  const terminalJobIds = useRef(new Set<string>());
  const activeJobId = useRef<string | null>(null);
  const deletingProjectIdRef = useRef<string | null>(null);
  const [dragState, setDragState] = useState<"idle" | "valid" | "invalid">("idle");
  const [error, setError] = useState<string | null>(null);
  const [projectPendingDeletion, setProjectPendingDeletion] = useState<string | null>(null);
  const [deletingProjectId, setDeletingProjectId] = useState<string | null>(null);
  const [renamingProjectId, setRenamingProjectId] = useState<string | null>(null);
  const [projectNameDraft, setProjectNameDraft] = useState("");
  const [contextMenuProjectId, setContextMenuProjectId] = useState<string | null>(null);
  const projectNameInputRef = useRef<HTMLInputElement>(null);
  const renameInFlightRef = useRef<string | null>(null);
  const cancelRenameOnBlurRef = useRef<string | null>(null);
  const recentContextMenu = useContextMenuTrigger();
  const recentProjects = useProjectStore((state) => state.recentProjects);
  const currentProjectId = useProjectStore((state) => state.details?.project.id);
  const recentProjectsCount = useSettingsStore((state) => state.value<number>("general.recentProjectsCount"));
  const visibleRecentProjects = recentProjects.slice(0, Math.max(0, recentProjectsCount));
  const setSourceJar = useProjectStore((state) => state.setSourceJar);
  const setScanFolderPath = useScanStore((state) => state.setFolderPath);
  const setScanCreationComplete = useScanStore((state) => state.setCreationComplete);
  const setAnalysisProgress = useProjectStore((state) => state.setAnalysisProgress);
  const setAnalysisJobId = useProjectStore((state) => state.setAnalysisJobId);
  const setAnalysisError = useProjectStore((state) => state.setAnalysisError);
  const setProjectDetails = useProjectStore((state) => state.setProjectDetails);
  const setModUnits = useProjectStore((state) => state.setModUnits);
  const setModImportFailures = useProjectStore((state) => state.setModImportFailures);
  const setRecentProjects = useProjectStore((state) => state.setRecentProjects);
  const setOperationProgress = useProjectStore((state) => state.setOperationProgress);
  const setOperationTitle = useProjectStore((state) => state.setOperationTitle);
  const projectOpenError = useProjectStore((state) => state.projectOpenError);
  const setProjectOpenError = useProjectStore((state) => state.setProjectOpenError);
  const goTo = useAppStore((state) => state.goTo);

  const cancelActiveAnalysis = useCallback(() => {
    const jobId = activeJobId.current ?? useProjectStore.getState().analysisJobId;
    activeJobId.current = null;
    if (jobId) void cancelAnalysis(jobId).catch(() => {});
  }, []);

  const switchProject = useCallback(async (
    details: NonNullable<ReturnType<typeof useProjectStore.getState>['details']>,
    generation: number,
    fromAnalysis = false,
  ) => {
    const previousProjectId = useProjectStore.getState().details?.project.id;
    if (previousProjectId !== details.project.id && useAppStore.getState().route === "editor") {
      if (!(await flushBeforeLeavingEditor())) return;
    }
    setProjectDetails(details);
    const modUnits = await listModUnits();
    if (generation !== currentAnalysisGeneration) return;
    setModUnits(modUnits);
    if (!details.setupCompleted) {
      setOperationProgress(null);
      // 画面離脱後もbackendのactive projectに合わせて結果を反映するが、Homeや設定を閉じない。
      // 結果取得中にも移動できるため、完了通知時ではなく遷移直前に現在のrouteを確認する。
      if (!fromAnalysis || useAppStore.getState().route === "analyze") await goTo("review");
      return;
    }
    setOperationProgress({ percent: null, message: "翻訳データを読み込んでいます" });
    const entries = await listEntries(modUnits[0]?.id ?? null);
    if (generation !== currentAnalysisGeneration) return;
    useEditorStore.getState().loadEntries(entries, details.project.id);
    setOperationProgress(null);
    if (!fromAnalysis || useAppStore.getState().route === "analyze") await goTo("editor");
  }, [goTo, setModUnits, setOperationProgress, setProjectDetails]);

  const resumeProject = useCallback(async (projectId: string) => {
    const generation = ++currentAnalysisGeneration;
    cancelActiveAnalysis();
    setError(null);
    setProjectOpenError(null);
    setScanCreationComplete(false);
    setModImportFailures([]);
    try {
      if (
        useAppStore.getState().route === "editor"
        && useProjectStore.getState().details?.project.id !== projectId
        && !(await flushBeforeLeavingEditor())
      ) return;
      setOperationTitle("プロジェクトを開いています");
      setOperationProgress({ percent: 0, message: "プロジェクトを開いています" });
      await goTo("loading");
      const details = await openProject(projectId, (progress) => setOperationProgress(progress));
      if (generation !== currentAnalysisGeneration) return;
      await switchProject(details, generation);
    } catch (cause: unknown) {
      if (generation === currentAnalysisGeneration) {
        setOperationProgress(null);
        setProjectOpenError(errorMessage(cause));
        await goTo("home");
      }
    }
  }, [cancelActiveAnalysis, goTo, setModImportFailures, setOperationProgress, setOperationTitle, setProjectOpenError, setScanCreationComplete, switchProject]);

  const handleAnalysisEvent = useCallback((event: AnalysisChannelEvent, generation: number) => {
    if (generation !== currentAnalysisGeneration) return;
    if (event.kind === "progress") {
      setAnalysisProgress({
        ...event.progress,
        modIndex: event.modIndex,
        modCount: event.modCount,
        modFilename: event.modFilename,
      });
      return;
    }
    if (event.kind === "failed") {
      terminalJobIds.current.add(event.jobId);
      if (activeJobId.current === event.jobId) activeJobId.current = null;
      setAnalysisError(event.error);
      setAnalysisJobId(null);
      return;
    }

    terminalJobIds.current.add(event.completed.jobId);
    if (activeJobId.current === event.completed.jobId) activeJobId.current = null;
    setAnalysisJobId(null);
    setModImportFailures(event.completed.failedMods);
    // create command側ですでにactive projectへ設定されるため、ここでは読み取りだけ行う。
    // 完了通知の遅延時に古いProjectをopenし直して、後から選んだProjectを上書きしない。
    void getProjectSummary()
      .then(async (details) => {
        if (generation !== currentAnalysisGeneration) return;
        await switchProject(details, generation, true);
      })
      .catch((cause: unknown) => {
        if (generation !== currentAnalysisGeneration) return;
        setAnalysisError({
          code: "project_open_failed",
          userMessage: errorMessage(cause),
          recoverable: true,
        });
      });
  }, [setAnalysisError, setAnalysisJobId, setAnalysisProgress, setModImportFailures, switchProject]);

  const handleJarPaths = useCallback((paths: string[]) => {
    const jarPaths = paths.filter(isJarFilePath);
    if (jarPaths.length === 0) {
      setError("JARファイルを選んでください。");
      return;
    }

    setError(null);
    setProjectOpenError(null);
    setScanCreationComplete(false);
    const generation = ++currentAnalysisGeneration;
    cancelActiveAnalysis();
    void (async () => {
      // 解析開始前に編集内容を保存し、別Projectへの切替で保存先が変わらないようにする。
      if (useAppStore.getState().route === "editor" && !(await flushBeforeLeavingEditor())) return;
      if (generation !== currentAnalysisGeneration) return;
      setSourceJar(jarPaths[0], jarPaths.map(fileNameFromPath).join(", "));
      await goTo("analyze");
      if (generation !== currentAnalysisGeneration) return;
      try {
        const jobId = await createProjectFromJars(jarPaths, null, (event) => handleAnalysisEvent(event, generation));
        if (generation !== currentAnalysisGeneration) {
          void cancelAnalysis(jobId).catch(() => {});
        } else if (!terminalJobIds.current.has(jobId)) {
          activeJobId.current = jobId;
          setAnalysisJobId(jobId);
        }
      } catch (cause: unknown) {
        if (generation !== currentAnalysisGeneration) return;
        setAnalysisError({
          code: cause instanceof AppCommandError ? cause.appError.code : "unexpected",
          userMessage: errorMessage(cause),
          recoverable: true,
        });
      }
    })();
  }, [cancelActiveAnalysis, goTo, handleAnalysisEvent, setAnalysisError, setAnalysisJobId, setProjectOpenError, setScanCreationComplete, setSourceJar]);

  const handleJarPath = useCallback((path: string) => handleJarPaths([path]), [handleJarPaths]);

  const handleModsFolder = useCallback(async (path: string) => {
    ++currentAnalysisGeneration;
    cancelActiveAnalysis();
    setError(null);
    setProjectOpenError(null);
    setAnalysisJobId(null);
    setAnalysisProgress(null);
    setAnalysisError(null);
    setModImportFailures([]);
    setScanFolderPath(path);
    await goTo("scan");
  }, [cancelActiveAnalysis, goTo, setAnalysisError, setAnalysisJobId, setAnalysisProgress, setModImportFailures, setProjectOpenError, setScanFolderPath]);

  const handleDroppedPaths = useCallback((paths: string[]) => {
    const jarPaths = paths.filter(isJarFilePath);
    if (jarPaths.length > 0) {
      handleJarPaths(jarPaths);
    } else if (paths.length === 1) {
      void handleModsFolder(paths[0]);
    } else {
      setError("JARファイルかフォルダを選んでください。");
    }
  }, [handleJarPaths, handleModsFolder]);

  const chooseJar = useCallback(async () => {
    setError(null);
    setProjectOpenError(null);
    try {
      const selectedPath = await openFileDialog({
        multiple: true,
        directory: false,
        filters: [{ name: "JARファイル", extensions: ["jar"] }],
      });
      if (typeof selectedPath === "string") handleJarPath(selectedPath);
      else if (selectedPath) handleJarPaths(selectedPath);
    } catch {
      setError("ファイルを選べませんでした。もう一度お試しください。");
    }
  }, [handleJarPath, handleJarPaths, setProjectOpenError]);

  const chooseModsFolder = useCallback(async () => {
    setError(null);
    setProjectOpenError(null);
    try {
      const selectedPath = await openFileDialog({ multiple: false, directory: true });
      const path = Array.isArray(selectedPath) ? selectedPath[0] : selectedPath;
      if (path) await handleModsFolder(path);
    } catch {
      setError("フォルダを選べませんでした。もう一度お試しください。");
    }
  }, [handleModsFolder, setProjectOpenError]);

  const refreshRecentProjects = useCallback(async () => {
    try {
      setRecentProjects(await listRecentProjects());
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    }
  }, [setRecentProjects]);

  const beginProjectRename = useCallback((projectId: string, displayName: string) => {
    cancelRenameOnBlurRef.current = null;
    setError(null);
    setProjectNameDraft(displayName);
    setRenamingProjectId(projectId);
  }, []);

  useEffect(() => {
    if (!renamingProjectId) return;
    projectNameInputRef.current?.focus();
    projectNameInputRef.current?.select();
  }, [renamingProjectId]);

  const commitProjectRename = useCallback(async (projectId: string) => {
    if (renameInFlightRef.current !== null || renamingProjectId !== projectId) return;
    renameInFlightRef.current = projectId;
    const originalName = useProjectStore.getState().recentProjects.find((project) => project.id === projectId)?.displayName;
    if (projectNameDraft.trim() === originalName) {
      setRenamingProjectId(null);
      renameInFlightRef.current = null;
      return;
    }
    setRenamingProjectId(null);
    try {
      const summary = await renameProject(projectId, projectNameDraft);
      const current = useProjectStore.getState();
      current.setRecentProjects(current.recentProjects.map((project) => project.id === projectId
        ? { ...project, displayName: summary.displayName }
        : project));
      if (current.details?.project.id === projectId) {
        current.setProjectDetails({ ...current.details, project: summary });
      }
    } catch (cause: unknown) {
      setError(errorMessage(cause));
      setRenamingProjectId(projectId);
    } finally {
      renameInFlightRef.current = null;
    }
  }, [projectNameDraft, renamingProjectId]);

  const recentMenuTriggerProps = (projectId: string) => ({
    onContextMenu: (event: MouseEvent<HTMLElement>) => {
      if (!shouldPreserveNativeContextMenu(event.target)) setContextMenuProjectId(projectId);
      recentContextMenu.triggerProps.onContextMenu(event);
    },
    onKeyDown: (event: KeyboardEvent<HTMLElement>) => {
      if (!((event.shiftKey && event.key === "F10") || event.key === "ContextMenu")) return;
      if (!shouldPreserveNativeContextMenu(event.target)) setContextMenuProjectId(projectId);
      recentContextMenu.triggerProps.onKeyDown(event);
    },
  });

  const removeRecentProject = useCallback(async (projectId: string) => {
    if (deletingProjectIdRef.current !== null) return;
    deletingProjectIdRef.current = projectId;
    setDeletingProjectId(projectId);
    try {
      await deleteProject(projectId);
      setRecentProjects(useProjectStore.getState().recentProjects.filter((project) => project.id !== projectId));
      await refreshRecentProjects();
    } catch (cause: unknown) {
      setError(errorMessage(cause));
    } finally {
      deletingProjectIdRef.current = null;
      setDeletingProjectId(null);
    }
  }, [refreshRecentProjects, setRecentProjects]);

  useEffect(() => {
    let active = true;
    // 解析完了後もHomeに残る場合、新しいProjectを最近の一覧から開けるよう再取得する。
    void listRecentProjects()
      .then((projects) => {
        if (active) setRecentProjects(projects);
      })
      .catch((cause: unknown) => {
        if (active) setError(errorMessage(cause));
      });
    return () => {
      active = false;
    };
  }, [currentProjectId, setRecentProjects]);

  useEffect(() => {
    if (!isTauri()) return;

    let active = true;
    let unlisten: (() => void) | undefined;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        switch (event.payload.type) {
          case "enter": {
            setDragState(event.payload.paths.some(isJarFilePath) || event.payload.paths.length === 1 ? "valid" : "invalid");
            break;
          }
          case "over":
            break;
          case "leave":
            setDragState("idle");
            break;
          case "drop": {
            setDragState("idle");
            handleDroppedPaths(event.payload.paths);
            break;
          }
        }
      })
      .then((stopListening) => {
        if (active) unlisten = stopListening;
        else stopListening();
      })
      .catch(() => {
        if (active) setError("ドラッグできませんでした。ファイルを選んでください。");
      });

    return () => {
      active = false;
      unlisten?.();
    };
  }, [handleDroppedPaths]);

  useEffect(() => {
    const openJarPicker = () => void chooseJar();
    window.addEventListener("yakucraft:open-jar", openJarPicker);
    return () => window.removeEventListener("yakucraft:open-jar", openJarPicker);
  }, [chooseJar]);

  return (
    <section
      className={`focus-page home-screen${dragState === "invalid" ? " home-screen--drag-invalid" : ""}`}
      aria-labelledby="home-title"
    >
      <DropArea isDragging={dragState === "valid"}>
        <h1 className="page-title" id="home-title">JAR またはフォルダをドロップ</h1>
        {dragState === "invalid" && (
          <output className="home-screen__drop-message" aria-live="polite">JARファイルかフォルダを選んでください。</output>
        )}
        <div className="home-screen__actions">
          <Tooltip content="JARを選ぶ" shortcut="Ctrl+O">
            <PrimaryButton
              data-testid="home-open-jar"
              onClick={() => void chooseJar()}
              aria-keyshortcuts="Control+O"
            >
              ファイルを選ぶ
            </PrimaryButton>
          </Tooltip>
          <SecondaryButton data-testid="home-open-mods-folder" onClick={() => void chooseModsFolder()}>
            フォルダから読み込む
          </SecondaryButton>
        </div>
      </DropArea>
      <UpdaterNotice />
      {(error ?? projectOpenError) && <p className="status-message" role="alert">{error ?? projectOpenError}</p>}
      {visibleRecentProjects.length > 0 && (
        <section className="recent-projects" aria-labelledby="recent-projects-title">
          <h2 id="recent-projects-title">最近のプロジェクト</h2>
          <ul className="recent-projects__list">
            {visibleRecentProjects.map((project) => {
              const total = project.translationProgress.translated + project.translationProgress.missing;
              return (
                <li className="recent-projects__item" key={project.id} tabIndex={-1} {...recentMenuTriggerProps(project.id)}>
                  {renamingProjectId === project.id ? (
                    <div className="recent-projects__open">
                      <input
                        ref={projectNameInputRef}
                        className="recent-projects__name-input"
                        type="text"
                        autoComplete="off"
                        autoCorrect="off"
                        autoCapitalize="off"
                        spellCheck={false}
                        aria-label="プロジェクト名"
                        value={projectNameDraft}
                        onChange={(event) => setProjectNameDraft(event.currentTarget.value)}
                        onBlur={() => {
                          if (cancelRenameOnBlurRef.current === project.id) {
                            cancelRenameOnBlurRef.current = null;
                            return;
                          }
                          void commitProjectRename(project.id);
                        }}
                        onKeyDown={(event) => {
                          if (isImeCompositionKeyDown(event)) {
                            event.stopPropagation();
                            return;
                          }
                          if (event.key === "Enter") {
                            event.preventDefault();
                            event.currentTarget.blur();
                          } else if (event.key === "Escape") {
                            event.preventDefault();
                            event.stopPropagation();
                            cancelRenameOnBlurRef.current = project.id;
                            setRenamingProjectId(null);
                          }
                        }}
                      />
                      <span className="recent-projects__meta">
                        {project.modVersion ?? "バージョン不明"} · {project.translationProgress.translated.toLocaleString("ja-JP")} / {total.toLocaleString("ja-JP")} 翻訳済み
                      </span>
                      <time className="recent-projects__date" dateTime={project.lastOpened}>
                        {new Date(project.lastOpened).toLocaleString("ja-JP")}
                      </time>
                    </div>
                  ) : (
                    <button
                      className="recent-projects__open"
                      disabled={deletingProjectId !== null}
                      onClick={() => void resumeProject(project.id)}
                    >
                      <span className="recent-projects__name">{project.displayName}</span>
                      <span className="recent-projects__meta">
                        {project.modVersion ?? "バージョン不明"} · {project.translationProgress.translated.toLocaleString("ja-JP")} / {total.toLocaleString("ja-JP")} 翻訳済み
                      </span>
                      <time className="recent-projects__date" dateTime={project.lastOpened}>
                        {new Date(project.lastOpened).toLocaleString("ja-JP")}
                      </time>
                    </button>
                  )}
                  {renamingProjectId !== project.id && (
                    <TertiaryButton
                      aria-label={`${project.displayName}を削除`}
                      disabled={deletingProjectId !== null}
                      onClick={() => setProjectPendingDeletion(project.id)}
                    >
                      削除
                    </TertiaryButton>
                  )}
                </li>
              );
            })}
          </ul>
          {recentContextMenu.position && contextMenuProjectId && (
            <ContextMenu
              label={`${recentProjects.find((project) => project.id === contextMenuProjectId)?.displayName ?? "プロジェクト"}の操作`}
              position={recentContextMenu.position}
              onClose={() => { recentContextMenu.close(); setContextMenuProjectId(null); }}
            >
              <ContextMenuItem
                disabled={deletingProjectId !== null}
                onClick={() => void resumeProject(contextMenuProjectId)}
              >
                開く
              </ContextMenuItem>
              <ContextMenuItem
                disabled={deletingProjectId !== null}
                onClick={() => {
                  const project = recentProjects.find((candidate) => candidate.id === contextMenuProjectId);
                  if (project) beginProjectRename(project.id, project.displayName);
                }}
              >
                名前を変更
              </ContextMenuItem>
              <ContextMenuSeparator />
              <ContextMenuItem
                className="context-menu__item--danger"
                disabled={deletingProjectId !== null}
                onClick={() => setProjectPendingDeletion(contextMenuProjectId)}
              >
                削除
              </ContextMenuItem>
            </ContextMenu>
          )}
        </section>
      )}
      <Dialog
        open={projectPendingDeletion !== null}
        title="このプロジェクトを削除しますか？"
        onClose={() => setProjectPendingDeletion(null)}
      >
        <p>翻訳データも削除されます。</p>
        <div className="home-screen__dialog-actions">
          <SecondaryButton disabled={deletingProjectId !== null} onClick={() => setProjectPendingDeletion(null)}>キャンセル</SecondaryButton>
          <DestructiveButton
            disabled={deletingProjectId !== null}
            onClick={() => {
              const projectId = projectPendingDeletion;
              setProjectPendingDeletion(null);
              if (projectId) void removeRecentProject(projectId);
            }}
          >
            削除
          </DestructiveButton>
        </div>
      </Dialog>
    </section>
  );
}
