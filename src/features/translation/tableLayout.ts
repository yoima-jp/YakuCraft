export type TableColumnId = "preview" | "key" | "source" | "target" | "state" | "namespace";

export const DEFAULT_COLUMN_WIDTHS: Record<TableColumnId, number> = {
  preview: 48,
  key: 220,
  source: 240,
  target: 280,
  state: 140,
  namespace: 170,
};

export const MIN_COLUMN_WIDTHS: Record<TableColumnId, number> = {
  preview: 44,
  key: 140,
  source: 120,
  target: 150,
  state: 104,
  namespace: 120,
};

export const MAX_COLUMN_WIDTH = 720;
const TABLE_SCROLLBAR_GUTTER_WIDTH = 20;

export type TableColumnWidths = Record<TableColumnId, number> & {
  userSizedColumns?: TableColumnId[];
};

const PRIMARY_FLEX_COLUMNS: TableColumnId[] = ["source", "target"];

export function normalizeColumnWidths(value: unknown): Record<TableColumnId, number> {
  const widths = { ...DEFAULT_COLUMN_WIDTHS };
  if (!value || typeof value !== "object") return widths;
  for (const column of Object.keys(DEFAULT_COLUMN_WIDTHS) as TableColumnId[]) {
    const candidate = (value as Record<string, unknown>)[column];
    if (typeof candidate === "number" && Number.isFinite(candidate)) {
      widths[column] = Math.min(MAX_COLUMN_WIDTH, Math.max(MIN_COLUMN_WIDTHS[column], candidate));
    }
  }
  return widths;
}

export function readUserSizedColumns(value: unknown): Set<TableColumnId> {
  if (!value || typeof value !== "object") return new Set();
  const storedValue = value as { userSizedColumns?: unknown } & Record<string, unknown>;
  const stored = storedValue.userSizedColumns;
  if (Array.isArray(stored)) {
    return new Set(stored.filter((column): column is TableColumnId =>
      typeof column === "string" && Object.hasOwn(DEFAULT_COLUMN_WIDTHS, column),
    ));
  }

  // 旧形式には手動調整列の一覧がないため、既定幅と異なる保存値を引き継ぐ。
  return new Set((Object.keys(DEFAULT_COLUMN_WIDTHS) as TableColumnId[]).filter((column) => {
    const width = storedValue[column];
    return typeof width === "number" && Number.isFinite(width) && width !== DEFAULT_COLUMN_WIDTHS[column];
  }));
}

export function storeColumnWidths(
  widths: Record<TableColumnId, number>,
  userSizedColumns: ReadonlySet<TableColumnId>,
): TableColumnWidths {
  return { ...widths, userSizedColumns: [...userSizedColumns] };
}

export function minimumResponsiveTableWidth(
  hasMultipleNamespaces: boolean,
  preferredWidths: Record<TableColumnId, number>,
  userSizedColumns: ReadonlySet<TableColumnId>,
) {
  const visibleColumns: TableColumnId[] = [
    "preview",
    "key",
    "source",
    "target",
    "state",
    ...(hasMultipleNamespaces ? ["namespace" as const] : []),
  ];
  const minimumColumnWidth = visibleColumns.reduce((sum, column) =>
    sum + (userSizedColumns.has(column) ? preferredWidths[column] : MIN_COLUMN_WIDTHS[column]),
  0);
  // スクロールバーの幅も確保し、Inspectorを狭めたときに表が数pxだけ横にはみ出すのを防ぐ。
  return minimumColumnWidth + TABLE_SCROLLBAR_GUTTER_WIDTH;
}

export function calculateResponsiveColumnWidths(
  availableWidth: number,
  preferredWidths: Record<TableColumnId, number>,
  userSizedColumns: ReadonlySet<TableColumnId>,
  visibleColumns: readonly TableColumnId[],
): Record<TableColumnId, number> {
  const widths = { ...preferredWidths };
  const visibleWidth = visibleColumns.reduce((sum, column) => sum + widths[column], 0);
  if (!Number.isFinite(availableWidth) || availableWidth <= 0 || visibleColumns.length === 0) return widths;

  const difference = availableWidth - visibleWidth;
  if (Math.abs(difference) < 1) return widths;

  // 翻訳領域を優先して伸縮させ、列幅を手動調整した列は画面幅の変化から守る。
  const primaryFlexibleColumns = visibleColumns.filter((column) =>
    PRIMARY_FLEX_COLUMNS.includes(column) && !userSizedColumns.has(column),
  );
  const flexibleColumns = primaryFlexibleColumns.length > 0
    ? primaryFlexibleColumns
    : visibleColumns.filter((column) => column !== "preview" && !userSizedColumns.has(column));
  // 未調整列がない場合は、画面幅が変わってもユーザー指定幅を維持する。
  if (flexibleColumns.length === 0) return widths;

  const secondaryColumns = visibleColumns.filter((column) =>
    !flexibleColumns.includes(column) && !userSizedColumns.has(column),
  );

  if (difference > 0) {
    // 伸縮列が上限へ届いた分は、残る未調整列へ回して広い画面でも右端を埋める。
    let remaining = difference;
    for (const group of [flexibleColumns, secondaryColumns]) {
      let candidates = [...group];
      while (remaining > 0.01 && candidates.length > 0) {
        const weight = candidates.reduce((sum, column) => sum + preferredWidths[column], 0);
        let expanded = 0;
        for (const column of candidates) {
          const capacity = MAX_COLUMN_WIDTH - widths[column];
          const amount = Math.min(capacity, remaining * preferredWidths[column] / weight);
          widths[column] += amount;
          expanded += amount;
        }
        if (expanded < 0.01) break;
        remaining -= expanded;
        candidates = candidates.filter((column) => widths[column] < MAX_COLUMN_WIDTH - 0.01);
      }
    }
    return widths;
  }

  let remaining = -difference;
  for (const group of [flexibleColumns, secondaryColumns]) {
    let candidates = [...group];
    while (remaining > 0.01 && candidates.length > 0) {
      const weight = candidates.reduce((sum, column) => sum + preferredWidths[column], 0);
      let reduced = 0;
      for (const column of candidates) {
        const capacity = widths[column] - MIN_COLUMN_WIDTHS[column];
        const amount = Math.min(capacity, remaining * preferredWidths[column] / weight);
        widths[column] -= amount;
        reduced += amount;
      }
      if (reduced < 0.01) break;
      remaining -= reduced;
      candidates = candidates.filter((column) => widths[column] > MIN_COLUMN_WIDTHS[column] + 0.01);
    }
  }
  return widths;
}
