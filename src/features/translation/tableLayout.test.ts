import { describe, expect, it } from "vitest";

import {
  calculateResponsiveColumnWidths,
  DEFAULT_COLUMN_WIDTHS,
  normalizeColumnWidths,
  readUserSizedColumns,
  storeColumnWidths,
  type TableColumnId,
} from "./tableLayout";

const columns = Object.keys(DEFAULT_COLUMN_WIDTHS) as TableColumnId[];
const singleNamespaceColumns = columns.filter((column) => column !== "namespace");

describe("responsive translation table columns", () => {
  it("Inspectorの開閉で使える幅を再配分し、手動調整した列幅を保つ", () => {
    const userSized = new Set<TableColumnId>(["key"]);
    const preferredWidths = { ...DEFAULT_COLUMN_WIDTHS, key: 360 };
    const workspaceWidth = 1200;
    const inspectorWidth = 320;
    const resizeHandleWidth = 12;
    const openWidth = workspaceWidth - inspectorWidth - resizeHandleWidth;
    const closedWidth = workspaceWidth;

    const open = calculateResponsiveColumnWidths(openWidth, preferredWidths, userSized, singleNamespaceColumns);
    const closed = calculateResponsiveColumnWidths(closedWidth, preferredWidths, userSized, singleNamespaceColumns);

    expect(open.key).toBe(preferredWidths.key);
    expect(closed.key).toBe(preferredWidths.key);
    expect(sumWidths(open, singleNamespaceColumns)).toBeCloseTo(openWidth);
    expect(sumWidths(closed, singleNamespaceColumns)).toBeCloseTo(closedWidth);
    expect(closed.source + closed.target).toBeGreaterThan(open.source + open.target);
  });

  it("利用可能幅より広い既定列は伸縮列を最小幅まで縮める", () => {
    const availableWidth = 700;
    const widths = calculateResponsiveColumnWidths(availableWidth, DEFAULT_COLUMN_WIDTHS, new Set(), columns);

    expect(sumWidths(widths, columns)).toBeCloseTo(availableWidth);
    expect(widths.preview).toBeGreaterThanOrEqual(44);
    expect(widths.key).toBeGreaterThanOrEqual(140);
    expect(widths.source).toBeGreaterThanOrEqual(120);
    expect(widths.target).toBeGreaterThanOrEqual(150);
    expect(widths.state).toBeGreaterThanOrEqual(104);
    expect(widths.namespace).toBeGreaterThanOrEqual(120);
  });

  it("手動調整した列幅と固定対象を一緒に保存できる", () => {
    const preferredWidths = { ...DEFAULT_COLUMN_WIDTHS, key: 360 };
    const stored = storeColumnWidths(preferredWidths, new Set<TableColumnId>(["key"]));

    expect(normalizeColumnWidths(stored).key).toBe(360);
    expect(readUserSizedColumns(stored)).toEqual(new Set<TableColumnId>(["key"]));
  });
});

function sumWidths(widths: Record<TableColumnId, number>, visibleColumns: readonly TableColumnId[]) {
  return visibleColumns.reduce((sum, column) => sum + widths[column], 0);
}
