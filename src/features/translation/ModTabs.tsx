import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import { createPortal } from "react-dom";
import { useVirtualizer } from "@tanstack/react-virtual";

import { DismissScopeContext, useDismiss } from "../../components/useDismiss";
import {
  ContextMenu,
  ContextMenuItem,
  ContextMenuSeparator,
  shouldPreserveNativeContextMenu,
  useContextMenuTrigger,
} from "../../components/ContextMenu";
import type { ModUnitSummaryDto } from "../../types/project";
import { getModIcon } from "./modTabCommands";
import { visibleModTabUnits } from "./modTabState";
import "./ModTabs.css";

/* oxlint-disable jsx-a11y/prefer-tag-over-role -- 幅変更操作を持つseparatorはhrで表せない。 */

export interface ModProgress {
  translated: number;
  total: number;
  needsReview: number;
}

interface ModTabsProps {
  projectId: string;
  units: ModUnitSummaryDto[];
  openIds: number[];
  pinnedIds: number[];
  activeUnitId: number;
  selectionDisabled?: boolean;
  actionsDisabled?: boolean;
  progressById: ReadonlyMap<number, ModProgress>;
  onSelect: (modUnitId: number) => void;
  onClose: (modUnitId: number) => void;
  onCloseOthers: (modUnitId: number) => void;
  onCloseRight: (modUnitId: number) => void;
  onTogglePin: (modUnitId: number) => void;
  onReorder: (modUnitId: number, insertionIndex: number, targetPinned: boolean) => boolean;
  onTranslateMissing: (modUnitId: number) => void;
  onRefreshModpack: (modUnitId: number) => void;
  onRemove: (modUnitId: number) => void;
  orientation?: "horizontal" | "vertical";
  sidebarWidth?: number;
  sidebarCollapsed?: boolean;
  onSidebarWidthChange?: (width: number) => void;
  onSidebarWidthCommit?: (width: number) => void;
  onToggleSidebarCollapsed?: () => void;
}

type SidebarRow = { type: "section"; id: string; label: string } | { type: "unit"; id: number; unit: ModUnitSummaryDto };
type TabDropPresentation = { targetId: number | string; edge: "before" | "after" };
type TabDropTarget = TabDropPresentation & { insertionIndex: number; pinned: boolean };
type TabPointerDrag = {
  pointerId: number;
  sourceId: number;
  startX: number;
  startY: number;
  clientX: number;
  clientY: number;
  active: boolean;
};

type ModSort = "name" | "missing" | "progress";

const NUMBER_FORMAT = new Intl.NumberFormat("ja-JP");
let iconCacheProjectId: string | null = null;
const iconUrls = new Map<number, string | null>();
const iconRequests = new Map<number, Promise<string | null>>();

export function setModIconCacheProject(projectId: string | null) {
  if (iconCacheProjectId === projectId) return;
  clearModIconCacheInUi();
  iconCacheProjectId = projectId;
}

export function clearModIconCacheInUi() {
  for (const oldUrl of iconUrls.values()) {
    if (oldUrl) URL.revokeObjectURL(oldUrl);
  }
  iconUrls.clear();
  iconRequests.clear();
}

function unitName(unit: ModUnitSummaryDto) {
  if (unit.sourceKind === "modpack") return "Modpack 固有（クエスト・KubeJS）";
  return unit.name?.trim() || unit.sourceFilename;
}

function useIconUrl(projectId: string, modUnitId: number) {
  const [loadedIcon, setLoadedIcon] = useState<{ projectId: string; url: string | null }>(() => ({
    projectId,
    url: iconCacheProjectId === projectId ? iconUrls.get(modUnitId) ?? null : null,
  }));

  useEffect(() => {
    setModIconCacheProject(projectId);

    let current = true;
    const cached = iconUrls.get(modUnitId);
    if (cached !== undefined) {
      return () => { current = false; };
    }

    let request = iconRequests.get(modUnitId);
    if (!request) {
      request = getModIcon(modUnitId)
        .then((bytes) => {
          if (!bytes || iconCacheProjectId !== projectId) return null;
          return URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/png" }));
        })
        .catch(() => null)
        .then((iconUrl) => {
          if (iconCacheProjectId !== projectId) {
            if (iconUrl) URL.revokeObjectURL(iconUrl);
            return null;
          }
          iconUrls.set(modUnitId, iconUrl);
          iconRequests.delete(modUnitId);
          return iconUrl;
        });
      iconRequests.set(modUnitId, request);
    }
    void request.then((iconUrl) => { if (current) setLoadedIcon({ projectId, url: iconUrl }); });
    return () => { current = false; };
  }, [modUnitId, projectId]);

  if (loadedIcon.projectId === projectId) return loadedIcon.url;
  return iconCacheProjectId === projectId ? iconUrls.get(modUnitId) ?? null : null;
}

function ModIcon({ projectId, unit, className = "" }: {
  projectId: string;
  unit: ModUnitSummaryDto;
  className?: string;
}) {
  const url = useIconUrl(projectId, unit.id);
  if (url) return <img className={`mod-tabs__icon${className ? ` ${className}` : ""}`} src={url} alt="" />;
  return (
    <span className={`mod-tabs__icon mod-tabs__icon--fallback${className ? ` ${className}` : ""}`} aria-hidden="true">
      {unitName(unit).slice(0, 1).toLocaleUpperCase("ja-JP")}
    </span>
  );
}

export function ModTabs({
  projectId,
  units,
  openIds,
  pinnedIds,
  activeUnitId,
  selectionDisabled = false,
  actionsDisabled = false,
  progressById,
  onSelect,
  onClose,
  onCloseOthers,
  onCloseRight,
  onTogglePin,
  onReorder,
  onTranslateMissing,
  onRefreshModpack,
  onRemove,
  orientation = "horizontal",
  sidebarWidth = 256,
  sidebarCollapsed = false,
  onSidebarWidthChange,
  onSidebarWidthCommit,
  onToggleSidebarCollapsed,
}: ModTabsProps) {
  const tabsRef = useRef<HTMLDivElement>(null);
  const sidebarRef = useRef<HTMLDivElement>(null);
  const measuredSidebarCollapsedRef = useRef(sidebarCollapsed);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDialogElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const [panelOpen, setPanelOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<ModSort>("name");
  const [incompleteOnly, setIncompleteOnly] = useState(false);
  const contextMenu = useContextMenuTrigger();
  const [contextMenuTarget, setContextMenuTarget] = useState<{ modUnitId: number; source: "tab" | "all" } | null>(null);
  const sidebarDragRef = useRef<{ pointerId: number; x: number; width: number; currentWidth: number } | null>(null);
  const tabDragRef = useRef<TabPointerDrag | null>(null);
  const tabDropTargetRef = useRef<TabDropTarget | null>(null);
  const autoScrollFrameRef = useRef<number | null>(null);
  const skipHorizontalScrollRef = useRef(false);
  const skipSidebarScrollRef = useRef(false);
  const [tabDropPresentation, setTabDropPresentation] = useState<TabDropPresentation | null>(null);
  const dismissRefs = useMemo(() => [triggerRef, panelRef], []);
  const shortcutDescriptionId = useId();

  const registerNestedRoot = useDismiss({
    open: panelOpen,
    refs: dismissRefs,
    // スクロールやリサイズでは閉じず、トリガーの位置に追従させる。
    dismissOnResize: false,
    dismissOnScroll: false,
    onDismiss: (reason) => {
      setPanelOpen(false);
      if (reason === "escape") triggerRef.current?.focus();
    },
  });

  useEffect(() => {
    if (panelOpen) {
      // 検索欄へのフォーカスでEditorをスクロールすると、スクロール検知がポップアップを閉じるため位置を保つ。
      searchRef.current?.focus({ preventScroll: true });
    }
  }, [panelOpen]);

  useEffect(() => {
    if (orientation === "vertical") setPanelOpen(false);
  }, [orientation]);

  // 両レイアウトで同じ順を使い、番号・循環ショートカットの対象を見た目に一致させる。
  const shownTabs = useMemo(
    () => visibleModTabUnits(units, openIds, pinnedIds),
    [openIds, pinnedIds, units],
  );
  const canCloseTab = shownTabs.length > 1;
  const contextTriggerProps = (modUnitId: number, source: "tab" | "all") => ({
    onContextMenu: (event: React.MouseEvent<HTMLElement>) => {
      if (!shouldPreserveNativeContextMenu(event.target)) setContextMenuTarget({ modUnitId, source });
      contextMenu.triggerProps.onContextMenu(event);
    },
    onKeyDown: (event: React.KeyboardEvent<HTMLElement>) => {
      if (!((event.shiftKey && event.key === "F10") || event.key === "ContextMenu")) return;
      if (!shouldPreserveNativeContextMenu(event.target)) setContextMenuTarget({ modUnitId, source });
      contextMenu.triggerProps.onKeyDown(event);
    },
  });

  // 全modを初期表示しても、画面内と少数の先読み分だけを描画する。
  // oxlint-disable-next-line react/incompatible-library -- TanStack Virtualは現在のスクロール位置を直接参照する。
  const tabVirtualizer = useVirtualizer({
    count: shownTabs.length,
    getScrollElement: () => tabsRef.current,
    getItemKey: (index) => shownTabs[index]?.id ?? index,
    estimateSize: () => 240,
    gap: 4,
    horizontal: true,
    overscan: 4,
  });

  // mod IDをキーに実測幅が保持されるため、並び替え時に全件のサイズキャッシュを消す必要はない。
  // measure()はキャッシュを消す一方、マウント済み要素のrefを再実行しないため、横タブは推定幅に戻ってしまう。
  useLayoutEffect(() => {
    if (skipHorizontalScrollRef.current) {
      skipHorizontalScrollRef.current = false;
      // Keep the drop position after edge auto-scroll instead of jumping back to an unchanged active tab.
      return;
    }
    const activeIndex = shownTabs.findIndex((unit) => unit.id === activeUnitId);
    if (activeIndex >= 0) tabVirtualizer.scrollToIndex(activeIndex, { align: "auto" });
  }, [activeUnitId, shownTabs, tabVirtualizer]);

  // TableとWorkspaceのoverflowで切れないようbodyへPortalし、アンカーとビューポートに合わせて固定配置する。
  useLayoutEffect(() => {
    if (!panelOpen) return;

    const positionPanel = () => {
      const trigger = triggerRef.current;
      const panel = panelRef.current;
      if (!trigger || !panel) return;

      const anchor = trigger.getBoundingClientRect();
      const bounds = panel.getBoundingClientRect();
      const gutter = 16;
      const viewportWidth = document.documentElement.clientWidth || window.innerWidth;
      const viewportHeight = document.documentElement.clientHeight || window.innerHeight;
      const left = Math.max(gutter, Math.min(anchor.right - bounds.width, viewportWidth - bounds.width - gutter));
      const below = anchor.bottom + gutter;
      const preferredTop = below + bounds.height <= viewportHeight - gutter
        ? below
        : anchor.top - bounds.height - gutter;
      const top = Math.max(gutter, Math.min(preferredTop, viewportHeight - bounds.height - gutter));
      panel.style.left = `${left}px`;
      panel.style.top = `${top}px`;
    };

    let frame = 0;
    const schedulePosition = () => {
      if (frame) cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        frame = 0;
        positionPanel();
      });
    };

    positionPanel();
    document.addEventListener("scroll", schedulePosition, true);
    window.addEventListener("resize", schedulePosition);
    window.visualViewport?.addEventListener("resize", schedulePosition);
    window.visualViewport?.addEventListener("scroll", schedulePosition);
    const resizeObserver = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedulePosition);
    if (resizeObserver) {
      if (triggerRef.current) resizeObserver.observe(triggerRef.current);
      if (panelRef.current) resizeObserver.observe(panelRef.current);
    }

    return () => {
      if (frame) cancelAnimationFrame(frame);
      document.removeEventListener("scroll", schedulePosition, true);
      window.removeEventListener("resize", schedulePosition);
      window.visualViewport?.removeEventListener("resize", schedulePosition);
      window.visualViewport?.removeEventListener("scroll", schedulePosition);
      resizeObserver?.disconnect();
    };
  }, [panelOpen]);

  const filteredUnits = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase("ja-JP");
    return units
      .filter((unit) => !incompleteOnly || (progressById.get(unit.id)?.translated ?? unit.translated) < (progressById.get(unit.id)?.total ?? unit.total))
      .filter((unit) => !normalizedQuery || unitName(unit).toLocaleLowerCase("ja-JP").includes(normalizedQuery))
      .sort((left, right) => {
        const leftProgress = progressById.get(left.id) ?? left;
        const rightProgress = progressById.get(right.id) ?? right;
        if (sort === "missing") {
          return (rightProgress.total - rightProgress.translated) - (leftProgress.total - leftProgress.translated)
            || unitName(left).localeCompare(unitName(right), "ja");
        }
        if (sort === "progress") {
          const leftRate = leftProgress.total === 0 ? 1 : leftProgress.translated / leftProgress.total;
          const rightRate = rightProgress.total === 0 ? 1 : rightProgress.translated / rightProgress.total;
          return leftRate - rightRate || unitName(left).localeCompare(unitName(right), "ja");
        }
        return unitName(left).localeCompare(unitName(right), "ja");
      });
  }, [incompleteOnly, progressById, query, sort, units]);

  const searchMode = query.trim().length > 0;
  const sidebarSearchUnits = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase("ja-JP");
    return units
      .filter((unit) => unitName(unit).toLocaleLowerCase("ja-JP").includes(normalizedQuery))
      .sort((left, right) => unitName(left).localeCompare(unitName(right), "ja"));
  }, [query, units]);
  const pinnedUnits = useMemo(() => shownTabs.filter((unit) => pinnedIds.includes(unit.id)), [pinnedIds, shownTabs]);
  const openUnits = useMemo(() => shownTabs.filter((unit) => !pinnedIds.includes(unit.id)), [pinnedIds, shownTabs]);
  const sidebarUnits = useMemo(
    () => searchMode ? sidebarSearchUnits : shownTabs,
    [searchMode, shownTabs, sidebarSearchUnits],
  );
  const sidebarRows = useMemo<SidebarRow[]>(() => {
    if (searchMode) return sidebarSearchUnits.map((unit) => ({ type: "unit", id: unit.id, unit }));
    if (sidebarCollapsed) return sidebarUnits.map((unit) => ({ type: "unit", id: unit.id, unit }));
    return [
      { type: "section", id: "pinned", label: "ピン留め" },
      ...pinnedUnits.map((unit) => ({ type: "unit" as const, id: unit.id, unit })),
      { type: "section", id: "open", label: "開いているタブ" },
      ...openUnits.map((unit) => ({ type: "unit" as const, id: unit.id, unit })),
    ];
  }, [openUnits, pinnedUnits, searchMode, sidebarCollapsed, sidebarSearchUnits, sidebarUnits]);

  const setDropTarget = (target: TabDropTarget | null) => {
    tabDropTargetRef.current = target;
    setTabDropPresentation((current) => {
      if (current?.targetId === target?.targetId && current?.edge === target?.edge) return current;
      return target ? { targetId: target.targetId, edge: target.edge } : null;
    });
  };

  const findDropTarget = (clientX: number, clientY: number): TabDropTarget | null => {
    const container = orientation === "horizontal" ? tabsRef.current : sidebarRef.current;
    if (!container || (orientation === "vertical" && searchMode)) return null;
    const bounds = container.getBoundingClientRect();
    if (clientX < bounds.left || clientX > bounds.right || clientY < bounds.top || clientY > bounds.bottom) return null;
    const orderedIndex = new Map(shownTabs.map((unit, index) => [unit.id, index]));
    const pinned = new Set(pinnedIds);
    const atGap = (rawGap: number, targetPinned: boolean, targetId: number | string, edge: TabDropTarget["edge"]): TabDropTarget => {
      const sourceIndex = shownTabs.findIndex((unit) => unit.id === tabDragRef.current?.sourceId);
      const insertionIndex = rawGap - (sourceIndex >= 0 && sourceIndex < rawGap ? 1 : 0);
      return { insertionIndex, pinned: targetPinned, targetId, edge };
    };

    if (orientation === "vertical") {
      const section = document.elementFromPoint(clientX, clientY)
        ?.closest<HTMLElement>(".mod-tabs__sidebar-section[data-mod-section]");
      const sectionId = section?.dataset.modSection;
      if (sectionId === "pinned" || sectionId === "open") {
        return atGap(
          sectionId === "pinned" ? 0 : pinnedUnits.length,
          sectionId === "pinned",
          sectionId,
          sectionId === "pinned" ? "after" : "before",
        );
      }
    }

    const selector = orientation === "horizontal"
      ? ".mod-tabs__item[data-mod-unit-id]"
      : ".mod-tabs__sidebar-row[data-mod-unit-id]";
    const candidates = Array.from(container.querySelectorAll<HTMLElement>(selector))
      .flatMap((element) => {
        const id = Number(element.dataset.modUnitId);
        const index = orderedIndex.get(id);
        if (index === undefined) return [];
        const rect = element.getBoundingClientRect();
        const center = orientation === "horizontal" ? rect.left + rect.width / 2 : rect.top + rect.height / 2;
        const point = orientation === "horizontal" ? clientX : clientY;
        return [{ id, index, rect, center, containsPoint: orientation === "horizontal"
          ? point >= rect.left && point <= rect.right
          : point >= rect.top && point <= rect.bottom }];
      })
      .sort((left, right) => left.center - right.center);
    if (candidates.length === 0) return null;

    const point = orientation === "horizontal" ? clientX : clientY;
    const hovered = candidates.find((candidate) => candidate.containsPoint);
    if (hovered) {
      const after = point >= hovered.center;
      return atGap(hovered.index + (after ? 1 : 0), pinned.has(hovered.id), hovered.id, after ? "after" : "before");
    }

    const next = candidates.find((candidate) => candidate.center >= point);
    if (next) {
      return atGap(next.index, pinned.has(next.id), next.id, "before");
    }
    const previous = candidates[candidates.length - 1];
    if (!previous) return null;
    return atGap(previous.index + 1, pinned.has(previous.id), previous.id, "after");
  };

  const beginTabDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || (orientation === "vertical" && searchMode)) return;
    const target = event.target instanceof Element
      ? event.target.closest<HTMLButtonElement>(".mod-tabs__tab, .mod-tabs__sidebar-tab")
      : null;
    const sourceId = Number(target?.dataset.modUnitId);
    if (!target || !Number.isSafeInteger(sourceId) || !shownTabs.some((unit) => unit.id === sourceId)) return;
    tabDragRef.current = {
      pointerId: event.pointerId,
      sourceId,
      startX: event.clientX,
      startY: event.clientY,
      clientX: event.clientX,
      clientY: event.clientY,
      active: false,
    };
  };

  const stopHorizontalAutoScroll = () => {
    if (autoScrollFrameRef.current === null) return;
    cancelAnimationFrame(autoScrollFrameRef.current);
    autoScrollFrameRef.current = null;
  };

  const autoScrollHorizontal = () => {
    const drag = tabDragRef.current;
    const strip = tabsRef.current;
    if (!drag?.active || orientation !== "horizontal" || !strip) {
      autoScrollFrameRef.current = null;
      return;
    }

    const bounds = strip.getBoundingClientRect();
    const edge = 40;
    const leftDistance = bounds.left + edge - drag.clientX;
    const rightDistance = drag.clientX - (bounds.right - edge);
    const delta = leftDistance > 0 ? -Math.min(18, Math.max(2, leftDistance / 3))
      : rightDistance > 0 ? Math.min(18, Math.max(2, rightDistance / 3))
        : 0;
    if (delta !== 0) {
      strip.scrollLeft += delta;
      setDropTarget(findDropTarget(drag.clientX, drag.clientY));
    }
    autoScrollFrameRef.current = requestAnimationFrame(autoScrollHorizontal);
  };

  const moveTabDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = tabDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    drag.clientX = event.clientX;
    drag.clientY = event.clientY;
    if (!drag.active && Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) >= 5) {
      drag.active = true;
      // Small pointer jitter remains a click; only larger movement starts reordering.
      // Capture keeps events flowing after the virtualized source leaves the hit-test path.
      // Pointer Events defines this retargeting behavior: https://www.w3.org/TR/pointerevents3/#pointer-capture
      event.currentTarget.setPointerCapture(event.pointerId);
      autoScrollFrameRef.current = requestAnimationFrame(autoScrollHorizontal);
    }
    if (!drag.active) return;
    event.preventDefault();
    setDropTarget(findDropTarget(event.clientX, event.clientY));
  };

  const finishTabDrag = (event: ReactPointerEvent<HTMLDivElement>, commit: boolean) => {
    const drag = tabDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    stopHorizontalAutoScroll();
    if (commit && drag.active) {
      event.preventDefault();
      const target = tabDropTargetRef.current;
      if (target && onReorder(drag.sourceId, target.insertionIndex, target.pinned)) {
        if (orientation === "horizontal") skipHorizontalScrollRef.current = true;
        else skipSidebarScrollRef.current = true;
      }
    }
    tabDragRef.current = null;
    setDropTarget(null);
  };

  useEffect(() => () => stopHorizontalAutoScroll(), []);

  // oxlint-disable-next-line react/incompatible-library -- 仮想スクロールはTanStack Virtualの現在値を毎回読む必要があり、memo化できない。
  const virtualizer = useVirtualizer({
    count: filteredUnits.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 76,
    overscan: 6,
    enabled: panelOpen,
  });

  // 左サイドバーには数百件のmodが並ぶため、テーブルと同じ仮想リストで描画件数を抑える。
  // oxlint-disable-next-line react/incompatible-library -- TanStack Virtualはスクロール状態を直接参照する。
  const sidebarVirtualizer = useVirtualizer({
    count: sidebarRows.length,
    getScrollElement: () => sidebarRef.current,
    getItemKey: (index) => {
      const row = sidebarRows[index];
      return row ? `${row.type}:${row.id}` : index;
    },
    estimateSize: (index) => sidebarRows[index]?.type === "section" ? 32 : sidebarCollapsed ? 52 : 72,
    overscan: 8,
    enabled: orientation === "vertical",
  });

  useLayoutEffect(() => {
    if (orientation !== "vertical") return;
    if (measuredSidebarCollapsedRef.current !== sidebarCollapsed) {
      // 折りたたみだけは行高が変わるため、古い高さを破棄して表示中の行を同期再計測する。
      sidebarVirtualizer.measure();
      measuredSidebarCollapsedRef.current = sidebarCollapsed;
      sidebarRef.current?.querySelectorAll<HTMLElement>("[data-index]").forEach((element) => {
        sidebarVirtualizer.measureElement(element);
      });
    }
    if (skipSidebarScrollRef.current) {
      skipSidebarScrollRef.current = false;
      // Reordering must not scroll the active icon back into view while preserving the drop position.
      return;
    }
    const activeIndex = sidebarRows.findIndex((row) => row.type === "unit" && row.unit.id === activeUnitId);
    if (activeIndex >= 0) sidebarVirtualizer.scrollToIndex(activeIndex, { align: "auto" });
  }, [activeUnitId, orientation, sidebarCollapsed, sidebarRows, sidebarVirtualizer]);

  const focusTab = (direction: "next" | "previous" | "first" | "last", current: HTMLButtonElement) => {
    if (shownTabs.length === 0) return;
    const currentId = Number(current.dataset.modUnitId);
    const index = shownTabs.findIndex((unit) => unit.id === currentId);
    if (index < 0) return;
    const nextIndex = direction === "first" ? 0 : direction === "last" ? shownTabs.length - 1
      : (index + (direction === "next" ? 1 : -1) + shownTabs.length) % shownTabs.length;
    const next = shownTabs[nextIndex];
    if (!next) return;
    tabVirtualizer.scrollToIndex(nextIndex, { align: "auto" });
    requestAnimationFrame(() => document.getElementById(`mod-tab-${next.id}`)?.focus());
  };

  const focusSidebarTab = (direction: "next" | "previous" | "first" | "last", current: HTMLButtonElement) => {
    if (sidebarUnits.length === 0) return;
    const currentId = Number(current.dataset.modUnitId);
    const index = sidebarUnits.findIndex((unit) => unit.id === currentId);
    if (index < 0) return;
    const nextIndex = direction === "first" ? 0 : direction === "last" ? sidebarUnits.length - 1
      : (index + (direction === "next" ? 1 : -1) + sidebarUnits.length) % sidebarUnits.length;
    const next = sidebarUnits[nextIndex];
    if (!next) return;
    const rowIndex = sidebarRows.findIndex((row) => row.type === "unit" && row.unit.id === next.id);
    if (rowIndex < 0) return;
    sidebarVirtualizer.scrollToIndex(rowIndex, { align: "auto" });
    requestAnimationFrame(() => document.getElementById(`mod-tab-${next.id}`)?.focus());
  };

  const resizeSidebar = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = sidebarDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    drag.currentWidth = Math.min(420, Math.max(200, drag.width + event.clientX - drag.x));
    onSidebarWidthChange?.(drag.currentWidth);
  };

  const stopSidebarResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = sidebarDragRef.current;
    if (drag?.pointerId !== event.pointerId) return;
    sidebarDragRef.current = null;
    onSidebarWidthCommit?.(drag.currentWidth);
  };

  const menuUnit = contextMenuTarget
    ? units.find((unit) => unit.id === contextMenuTarget.modUnitId)
    : undefined;
  const menuTabIndex = menuUnit
    ? orientation === "vertical"
      ? sidebarUnits.findIndex((unit) => unit.id === menuUnit.id)
      : shownTabs.findIndex((unit) => unit.id === menuUnit.id)
    : -1;
  const hasTabToRight = orientation === "vertical"
    ? menuTabIndex >= 0 && menuTabIndex < sidebarUnits.length - 1
    : menuTabIndex >= 0 && menuTabIndex < shownTabs.length - 1;

  return (
    <DismissScopeContext.Provider value={registerNestedRoot}>
    <div className={`mod-tabs${orientation === "vertical" ? " mod-tabs--vertical" : ""}`}>
      {/* 横タブはサイドバー検索中も表示され続け、aria-describedby で常にこの要素を参照するため、検索状態に関係なく描画する。 */}
      <span id={shortcutDescriptionId} className="visually-hidden">
        Ctrl+1〜Ctrl+8で番号のタブへ、Ctrl+9で最後へ移動。Ctrl+Shift+PageUp/PageDownでアクティブタブを移動
      </span>
      {orientation === "vertical" && (
        <>
          <div
            className="mod-tabs__sidebar"
            data-collapsed={sidebarCollapsed}
            style={{ "--mod-sidebar-width": sidebarCollapsed ? "3.5rem" : `${sidebarWidth}px` } as CSSProperties}
          >
            <div className="mod-tabs__sidebar-header">
              {!sidebarCollapsed && (
                <label className="mod-tabs__sidebar-search-label">
                  <span className="visually-hidden">mod名で検索</span>
                  <input
                    className="mod-tabs__search"
                    type="search"
                    autoComplete="off"
                    autoCorrect="off"
                    autoCapitalize="off"
                    spellCheck={false}
                    value={query}
                    onChange={(event) => setQuery(event.currentTarget.value)}
                    placeholder="modを検索"
                  />
                </label>
              )}
              <button
                className="mod-tabs__collapse"
                type="button"
                aria-label={sidebarCollapsed ? "mod一覧を展開" : "mod一覧を折りたたむ"}
                onClick={onToggleSidebarCollapsed}
              >
                <span aria-hidden="true">{sidebarCollapsed ? "›" : "‹"}</span>
              </button>
            </div>
            <div
              className="mod-tabs__sidebar-list"
              ref={sidebarRef}
              role="tablist"
              aria-label={searchMode ? "mod検索結果" : "開いているmod"}
              aria-describedby={searchMode ? undefined : shortcutDescriptionId}
              aria-keyshortcuts={searchMode ? undefined : "Control+1 Control+2 Control+3 Control+4 Control+5 Control+6 Control+7 Control+8 Control+9 Control+Shift+PageUp Control+Shift+PageDown"}
              aria-orientation="vertical"
              onPointerDown={beginTabDrag}
              onPointerMove={moveTabDrag}
              onPointerUp={(event) => finishTabDrag(event, true)}
              onPointerCancel={(event) => finishTabDrag(event, false)}
            >
              {sidebarRows.length === 0 ? (
                <p className="mod-tabs__empty">modが見つかりません</p>
              ) : (
                <ul
                  className="mod-tabs__sidebar-items"
                  role="presentation"
                  style={{ height: sidebarVirtualizer.getTotalSize(), position: "relative" }}
                >
                  {sidebarVirtualizer.getVirtualItems().map((item) => {
                    const row = sidebarRows[item.index];
                    if (!row) return null;
                    if (row.type === "section") {
                      return (
                        <li
                          className="mod-tabs__sidebar-section"
                          role="presentation"
                          key={row.id}
                          data-mod-section={row.id}
                          data-drop-position={tabDropPresentation?.targetId === row.id ? tabDropPresentation.edge : undefined}
                          data-index={item.index}
                          ref={sidebarVirtualizer.measureElement}
                          style={{ transform: `translateY(${item.start}px)` }}
                        >
                          {row.label}
                        </li>
                      );
                    }
                    const unit = row.unit;
                    const progress = progressById.get(unit.id) ?? unit;
                    const name = unitName(unit);
                    const selected = unit.id === activeUnitId;
                    const rate = progress.total === 0 ? 0 : Math.round(progress.translated / progress.total * 100);
                    return (
                      <li
                        className="mod-tabs__sidebar-row"
                        role="presentation"
                        {...contextTriggerProps(unit.id, searchMode ? "all" : "tab")}
                        key={unit.id}
                        data-mod-unit-id={unit.id}
                        data-drop-position={tabDropPresentation?.targetId === unit.id ? tabDropPresentation.edge : undefined}
                        data-index={item.index}
                        ref={sidebarVirtualizer.measureElement}
                        style={{ transform: `translateY(${item.start}px)` }}
                      >
                        <button
                          className="mod-tabs__sidebar-tab"
                          id={`mod-tab-${unit.id}`}
                          data-mod-unit-id={unit.id}
                          type="button"
                          role="tab"
                          disabled={selectionDisabled}
                          aria-label={`${name}、${NUMBER_FORMAT.format(progress.translated)} / ${NUMBER_FORMAT.format(progress.total)} 翻訳済み`}
                          aria-selected={selected}
                          aria-controls="translation-table-panel"
                          title={sidebarCollapsed ? name : undefined}
                          tabIndex={selected ? 0 : -1}
                          onClick={() => {
                            onSelect(unit.id);
                          }}
                          onKeyDown={(event) => {
                            if (event.key === "ArrowDown") focusSidebarTab("next", event.currentTarget);
                            else if (event.key === "ArrowUp") focusSidebarTab("previous", event.currentTarget);
                            else if (event.key === "Home") focusSidebarTab("first", event.currentTarget);
                            else if (event.key === "End") focusSidebarTab("last", event.currentTarget);
                            else if (event.key === "Enter") {
                              event.preventDefault();
                              onSelect(unit.id);
                              return;
                            } else return;
                            event.preventDefault();
                          }}
                        >
                          <ModIcon projectId={projectId} unit={unit} />
                          <span className="mod-tabs__sidebar-info">
                            <span className="mod-tabs__unit-name">{name}</span>
                            <span className="mod-tabs__sidebar-progress" aria-hidden="true">
                              <span style={{ inlineSize: `${rate}%` }} />
                            </span>
                            <span className="mod-tabs__unit-meta">
                              {NUMBER_FORMAT.format(progress.translated)} / {NUMBER_FORMAT.format(progress.total)}
                            </span>
                          </span>
                        </button>
                        {!searchMode && (
                          <button
                            className="mod-tabs__close"
                            type="button"
                            disabled={!canCloseTab || selectionDisabled}
                            aria-label={`${name}を閉じる`}
                            onClick={() => onClose(unit.id)}
                          >
                            <span aria-hidden="true">×</span>
                          </button>
                        )}
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </div>
          <div
            className="mod-tabs__sidebar-resize"
            role="separator"
            aria-label="mod一覧の幅"
            aria-orientation="vertical"
            aria-valuemin={200}
            aria-valuemax={420}
            aria-valuenow={sidebarWidth}
            aria-valuetext={`${sidebarWidth} ピクセル`}
            aria-keyshortcuts="Alt+ArrowLeft Alt+ArrowRight"
            tabIndex={0}
            onPointerDown={(event) => {
              event.preventDefault();
              sidebarDragRef.current = { pointerId: event.pointerId, x: event.clientX, width: sidebarWidth, currentWidth: sidebarWidth };
              event.currentTarget.setPointerCapture?.(event.pointerId);
            }}
            onPointerMove={resizeSidebar}
            onPointerUp={stopSidebarResize}
            onPointerCancel={stopSidebarResize}
            onKeyDown={(event) => {
              if (!event.altKey || (event.key !== "ArrowLeft" && event.key !== "ArrowRight")) return;
              event.preventDefault();
              const nextWidth = Math.min(420, Math.max(200, sidebarWidth + (event.key === "ArrowRight" ? 16 : -16)));
              onSidebarWidthChange?.(nextWidth);
              onSidebarWidthCommit?.(nextWidth);
            }}
          />
        </>
      )}
      {orientation === "horizontal" && <>
      <div
        className="mod-tabs__strip"
        ref={tabsRef}
        role="tablist"
        aria-label="開いているmod"
        aria-describedby={shortcutDescriptionId}
        aria-keyshortcuts="Control+1 Control+2 Control+3 Control+4 Control+5 Control+6 Control+7 Control+8 Control+9 Control+Shift+PageUp Control+Shift+PageDown"
        onPointerDown={beginTabDrag}
        onPointerMove={moveTabDrag}
        onPointerUp={(event) => finishTabDrag(event, true)}
        onPointerCancel={(event) => finishTabDrag(event, false)}
        onWheel={(event) => {
          if (event.deltaY === 0 || Math.abs(event.deltaY) < Math.abs(event.deltaX)) return;
          if (event.currentTarget.scrollWidth <= event.currentTarget.clientWidth) return;
          event.preventDefault();
          event.currentTarget.scrollLeft += event.deltaY;
        }}
      >
        <div
          className="mod-tabs__virtual-strip"
          style={{ width: tabVirtualizer.getTotalSize(), position: "relative" }}
        >
          {tabVirtualizer.getVirtualItems().map((item) => {
            const unit = shownTabs[item.index];
            if (!unit) return null;
            const progress = progressById.get(unit.id) ?? unit;
            const name = unitName(unit);
            const selected = unit.id === activeUnitId;
            return (
              <div
                className="mod-tabs__item"
                role="presentation"
                tabIndex={-1}
                {...contextTriggerProps(unit.id, "tab")}
                key={unit.id}
                data-mod-unit-id={unit.id}
                data-drop-position={tabDropPresentation?.targetId === unit.id ? tabDropPresentation.edge : undefined}
                data-index={item.index}
                ref={tabVirtualizer.measureElement}
                style={{ transform: `translateX(${item.start}px)` }}
              >
                <button
                  className="mod-tabs__tab"
                  id={`mod-tab-${unit.id}`}
                  data-mod-unit-id={unit.id}
                  type="button"
                  role="tab"
                  disabled={selectionDisabled}
                  aria-selected={selected}
                  aria-controls="translation-table-panel"
                  tabIndex={selected ? 0 : -1}
                  onClick={() => onSelect(unit.id)}
                  onKeyDown={(event) => {
                    if (event.key === "ArrowRight") focusTab("next", event.currentTarget);
                    else if (event.key === "ArrowLeft") focusTab("previous", event.currentTarget);
                    else if (event.key === "Home") focusTab("first", event.currentTarget);
                    else if (event.key === "End") focusTab("last", event.currentTarget);
                    else return;
                    event.preventDefault();
                  }}
                >
                  <ModIcon projectId={projectId} unit={unit} />
                  <span className="mod-tabs__name">{name}</span>
                  <span className="mod-tabs__progress">
                    {NUMBER_FORMAT.format(progress.translated)} / {NUMBER_FORMAT.format(progress.total)}
                    <span className="visually-hidden"> 翻訳済み</span>
                  </span>
                </button>
                <button
                  className="mod-tabs__close"
                  type="button"
                  disabled={!canCloseTab || selectionDisabled}
                  aria-label={`${name}を閉じる`}
                  onClick={() => onClose(unit.id)}
                >
                  <span aria-hidden="true">×</span>
                </button>
              </div>
            );
          })}
        </div>
      </div>
      <div className="mod-tabs__all">
        <button
          className="mod-tabs__all-button"
          type="button"
          ref={triggerRef}
          aria-expanded={panelOpen}
          aria-controls={panelOpen ? "mod-tabs-all-panel" : undefined}
          onClick={() => setPanelOpen((open) => !open)}
        >
          すべてのmod
        </button>
        {panelOpen && createPortal(
          <dialog className="mod-tabs__panel" id="mod-tabs-all-panel" ref={panelRef} open aria-label="すべてのmod">
            <label className="mod-tabs__search-label">
              <span className="visually-hidden">mod名で検索</span>
              <input
                className="mod-tabs__search"
                type="search"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                ref={searchRef}
                value={query}
                onChange={(event) => setQuery(event.currentTarget.value)}
                placeholder="mod名で検索"
              />
            </label>
            <label className="mod-tabs__sort-label">
              <span>並び替え</span>
              <select value={sort} onChange={(event) => setSort(event.currentTarget.value as ModSort)}>
                <option value="name">名前順</option>
                <option value="missing">未翻訳が多い順</option>
                <option value="progress">進捗が低い順</option>
              </select>
            </label>
            <label className="mod-tabs__incomplete">
              <input type="checkbox" checked={incompleteOnly} onChange={(event) => setIncompleteOnly(event.currentTarget.checked)} />
              未完了だけ
            </label>
            <div className="mod-tabs__list" ref={listRef}>
              {filteredUnits.length === 0 ? (
                <p className="mod-tabs__empty">modが見つかりません</p>
              ) : (
                <ul
                  className="mod-tabs__virtual-items"
                  aria-label="mod一覧"
                  style={{ height: virtualizer.getTotalSize(), position: "relative" }}
                >
                  {virtualizer.getVirtualItems().map((item) => {
                    const unit = filteredUnits[item.index];
                    if (!unit) return null;
                    const progress = progressById.get(unit.id) ?? unit;
                    const name = unitName(unit);
                    const rate = progress.total === 0 ? 0 : Math.round(progress.translated / progress.total * 100);
                    const pinned = pinnedIds.includes(unit.id);
                    return (
                      <li
                        className="mod-tabs__row"
                        tabIndex={-1}
                        {...contextTriggerProps(unit.id, "all")}
                        key={unit.id}
                        data-index={item.index}
                        ref={virtualizer.measureElement}
                        style={{ transform: `translateY(${item.start}px)` }}
                      >
                        <button
                          className="mod-tabs__open-unit"
                          type="button"
                          disabled={selectionDisabled}
                          onClick={() => { onSelect(unit.id); setPanelOpen(false); }}
                        >
                          <ModIcon projectId={projectId} unit={unit} />
                          <span className="mod-tabs__unit-info">
                            <span className="mod-tabs__unit-name">{name}</span>
                            <span className="mod-tabs__unit-meta">
                              {unit.version || "バージョン不明"} · {NUMBER_FORMAT.format(progress.translated)} / {NUMBER_FORMAT.format(progress.total)} · {rate}%
                            </span>
                          </span>
                        </button>
                        <button
                          className="mod-tabs__pin"
                          type="button"
                          aria-label={pinned ? `${name}のピン留めを解除` : `${name}をピン留め`}
                          aria-pressed={pinned}
                          onClick={() => onTogglePin(unit.id)}
                        >
                          <span aria-hidden="true">{pinned ? "★" : "☆"}</span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </dialog>,
          document.body,
        )}
      </div>
      </>}
      {menuUnit && contextMenu.position && (
        <ModUnitContextMenu
          name={unitName(menuUnit)}
          position={contextMenu.position}
          onClose={() => { contextMenu.close(); setContextMenuTarget(null); }}
          pinned={pinnedIds.includes(menuUnit.id)}
          canCloseTab={contextMenuTarget?.source === "tab" && canCloseTab}
          canCloseOthers={contextMenuTarget?.source === "tab" && shownTabs.length > 1}
          canCloseRight={contextMenuTarget?.source === "tab" && hasTabToRight}
          canRemove={units.length > 1}
          actionsDisabled={actionsDisabled}
          selectionDisabled={selectionDisabled}
          showTabActions={contextMenuTarget?.source === "tab"}
          onTranslate={() => onTranslateMissing(menuUnit.id)}
          showRefreshModpack={menuUnit.sourceKind === "modpack" && menuUnit.id === activeUnitId}
          onRefreshModpack={() => onRefreshModpack(menuUnit.id)}
          onTogglePin={() => onTogglePin(menuUnit.id)}
          onCloseTab={() => onClose(menuUnit.id)}
          onCloseOthers={() => onCloseOthers(menuUnit.id)}
          onCloseRight={() => onCloseRight(menuUnit.id)}
          onRemove={() => { onRemove(menuUnit.id); setPanelOpen(false); }}
        />
      )}
    </div>
    </DismissScopeContext.Provider>
  );
}

function ModUnitContextMenu({
  name,
  position,
  onClose,
  pinned,
  canCloseTab = false,
  canCloseOthers = false,
  canCloseRight = false,
  canRemove,
  actionsDisabled,
  selectionDisabled = false,
  showTabActions = false,
  showRefreshModpack = false,
  onTranslate,
  onRefreshModpack,
  onTogglePin,
  onCloseTab,
  onCloseOthers,
  onCloseRight,
  onRemove,
}: {
  name: string;
  position: NonNullable<ReturnType<typeof useContextMenuTrigger>["position"]>;
  onClose: () => void;
  pinned: boolean;
  canCloseTab?: boolean;
  canCloseOthers?: boolean;
  canCloseRight?: boolean;
  canRemove: boolean;
  actionsDisabled: boolean;
  selectionDisabled?: boolean;
  showTabActions?: boolean;
  showRefreshModpack?: boolean;
  onTranslate: () => void;
  onRefreshModpack: () => void;
  onTogglePin: () => void;
  onCloseTab?: () => void;
  onCloseOthers?: () => void;
  onCloseRight?: () => void;
  onRemove: () => void;
}) {
  return (
    <ContextMenu label={`${name}の操作`} position={position} onClose={onClose}>
      <ContextMenuItem disabled={actionsDisabled} onClick={onTranslate}>このmodの未翻訳をAIで翻訳</ContextMenuItem>
      {showRefreshModpack && (
        <ContextMenuItem disabled={actionsDisabled} onClick={onRefreshModpack}>Modpack を再取り込み</ContextMenuItem>
      )}
      <ContextMenuItem onClick={onTogglePin}>{pinned ? "ピン留めを外す" : "ピン留め"}</ContextMenuItem>
      {showTabActions && (
        <>
          <ContextMenuItem disabled={!canCloseTab || selectionDisabled} onClick={onCloseTab}>閉じる</ContextMenuItem>
          <ContextMenuItem disabled={!canCloseOthers || selectionDisabled} onClick={onCloseOthers}>他のタブを閉じる</ContextMenuItem>
          <ContextMenuItem disabled={!canCloseRight || selectionDisabled} onClick={onCloseRight}>右側のタブを閉じる</ContextMenuItem>
        </>
      )}
      <ContextMenuSeparator />
      <ContextMenuItem className="context-menu__item--danger" disabled={!canRemove || actionsDisabled} onClick={onRemove}>
        プロジェクトから外す
      </ContextMenuItem>
    </ContextMenu>
  );
}
