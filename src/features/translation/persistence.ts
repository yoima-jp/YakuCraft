import type { TranslationEntry } from "../../types/translation";
import { updateEntriesBatch } from "../../lib/tauri";

export interface EditorPersistence {
  save(entries: readonly TranslationEntry[]): Promise<TranslationEntry[]>;
}

export class TauriEditorPersistence implements EditorPersistence {
  save(entries: readonly TranslationEntry[]) {
    return updateEntriesBatch(entries.map(({ id, targetText }) => ({ id, targetText })));
  }
}

export interface MockEditorPersistenceOptions {
  delayMs?: number;
  failuresRemaining?: number;
}

export class MockEditorPersistence implements EditorPersistence {
  private failuresRemaining: number;
  readonly savedEntries = new Map<number, TranslationEntry>();

  constructor(private readonly options: MockEditorPersistenceOptions = {}) {
    this.failuresRemaining = options.failuresRemaining ?? 0;
  }

  async save(entries: readonly TranslationEntry[]) {
    const delayMs = this.options.delayMs ?? 90;
    if (delayMs > 0) {
      await new Promise<void>((resolve) => window.setTimeout(resolve, delayMs));
    }
    if (this.failuresRemaining > 0) {
      this.failuresRemaining -= 1;
      throw new Error("モック保存に失敗しました");
    }
    for (const entry of entries) this.savedEntries.set(entry.id, entry);
    return [...entries];
  }
}
