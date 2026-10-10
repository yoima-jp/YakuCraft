export interface ModTabPreferences {
  openIds: number[];
  pinnedIds: number[];
  activeId: number;
}

export function visibleModTabUnits<T extends { id: number }>(
  units: readonly T[],
  openIds: readonly number[],
  pinnedIds: readonly number[],
): T[] {
  const unitsById = new Map(units.map((unit) => [unit.id, unit]));
  const added = new Set<number>();
  const append = (ids: readonly number[]) => ids.flatMap((id) => {
    const unit = unitsById.get(id);
    if (!unit || added.has(id)) return [];
    added.add(id);
    return [unit];
  });
  const open = new Set(openIds);
  const pinned = new Set(pinnedIds);
  return [
    ...append(pinnedIds),
    ...append(openIds.filter((id) => open.has(id) && !pinned.has(id))),
  ];
}

export function reorderModTabPreferences(
  preferences: ModTabPreferences,
  orderedIds: readonly number[],
  sourceId: number,
  insertionIndex: number,
  targetPinned: boolean,
): ModTabPreferences {
  const sourceIndex = orderedIds.indexOf(sourceId);
  if (sourceIndex < 0) return preferences;

  // insertionIndex is a gap in the list after sourceId has been removed; the active tab stays unchanged.
  const nextOrder = orderedIds.filter((id) => id !== sourceId);
  const destinationIndex = Math.max(0, Math.min(nextOrder.length, insertionIndex));
  nextOrder.splice(destinationIndex, 0, sourceId);

  const pinned = new Set(preferences.pinnedIds);
  if (targetPinned) pinned.add(sourceId);
  else pinned.delete(sourceId);

  const open = new Set(preferences.openIds);
  // A pin-only tab must join the open list when a drag crosses into the open section.
  if (!targetPinned) open.add(sourceId);

  const pinnedIds = nextOrder.filter((id) => pinned.has(id));
  const openIds = nextOrder.filter((id) => open.has(id));
  if (
    arraysEqual(preferences.openIds, openIds)
    && arraysEqual(preferences.pinnedIds, pinnedIds)
  ) {
    return preferences;
  }
  return { ...preferences, openIds, pinnedIds };
}

function arraysEqual(left: readonly number[], right: readonly number[]) {
  return left.length === right.length && left.every((id, index) => id === right[index]);
}

export function modTabShortcutIndex(key: string, tabCount: number, activeIndex: number, reverse = false) {
  if (key === "Tab") {
    if (tabCount < 2) return null;
    return (activeIndex + (reverse ? -1 : 1) + tabCount) % tabCount;
  }

  if (!/^[1-9]$/.test(key)) return null;
  const index = key === "9" ? tabCount - 1 : Number(key) - 1;
  return index >= 0 && index < tabCount ? index : null;
}

function storageKey(projectId: string) {
  return `yakucraft:editor-mod-tabs:${projectId}`;
}

export function readModTabPreferences(projectId: string, validIds: number[]): ModTabPreferences {
  const fallbackId = validIds[0] ?? 0;
  const defaultPreferences = (): ModTabPreferences => ({
    openIds: [...validIds],
    pinnedIds: [],
    activeId: fallbackId,
  });
  if (validIds.length === 0 || typeof localStorage === "undefined") return defaultPreferences();

  try {
    const stored = localStorage.getItem(storageKey(projectId));
    if (stored === null) return defaultPreferences();

    const value: unknown = JSON.parse(stored);
    if (typeof value !== "object" || value === null) {
      return defaultPreferences();
    }
    const record = value as Record<string, unknown>;
    const allowed = new Set(validIds);
    const validList = (items: unknown) => Array.isArray(items)
      ? [...new Set(items.filter((id): id is number => Number.isSafeInteger(id) && allowed.has(id)))]
      : [];
    const openIds = validList(record.openIds);
    const pinnedIds = validList(record.pinnedIds);
    const activeId = typeof record.activeId === "number" && allowed.has(record.activeId)
      ? record.activeId
      : openIds[0] ?? pinnedIds[0] ?? fallbackId;
    if (!openIds.includes(activeId) && !pinnedIds.includes(activeId)) openIds.push(activeId);
    if (openIds.length === 0 && pinnedIds.length === 0) openIds.push(fallbackId);
    return { openIds, pinnedIds, activeId };
  } catch {
    return defaultPreferences();
  }
}

export function writeModTabPreferences(projectId: string, preferences: ModTabPreferences) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(storageKey(projectId), JSON.stringify(preferences));
  } catch {
    // localStorageが利用できない環境でも、タブ切り替えは続けられる。
  }
}
