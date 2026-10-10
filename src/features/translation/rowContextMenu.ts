export function prepareRowContextSelection(
  entryId: number,
  selectedIds: readonly number[],
  selectEntry: (entryId: number, mode: "replace") => void,
) {
  if (selectedIds.includes(entryId)) return [...selectedIds];
  selectEntry(entryId, "replace");
  return [entryId];
}
