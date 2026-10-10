import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getAiSettings, saveSettings, startAiJob } from "../../lib/tauri";
import { useDialogConfirmation } from "../../components/useDialogConfirmation";
import { useAiJobStore } from "../../stores/aiJobStore";
import { useEditorStore } from "../../stores/editorStore";
import { useProjectStore } from "../../stores/projectStore";
import { SETTING_DEFAULTS, useSettingsStore } from "../../stores/settingsStore";
import type { ProjectDetailsDto } from "../../types/project";
import type { TranslationEntry } from "../../types/translation";
import { translateWithAi } from "./aiTranslation";

vi.mock("../../lib/tauri", () => ({
  AppCommandError: class extends Error {},
  entryFromProject: vi.fn(),
  getAiSettings: vi.fn(),
  saveSettings: vi.fn(),
  startAiJob: vi.fn(),
}));

vi.mock("../../lib/navigation", () => ({ flushBeforeLeavingEditor: vi.fn(async () => true) }));

const entry: TranslationEntry = {
  id: 1,
  namespace: "sample",
  key: "item.apple",
  sourceText: "Apple",
  sourceLocale: "en_us",
  targetText: "りんご",
  origin: "existing",
  flags: [],
};

function ConfirmationHarness() {
  const { requestConfirmation, dialog } = useDialogConfirmation();
  return (
    <>
      <button type="button" onClick={() => void translateWithAi([1], requestConfirmation)}>AI翻訳</button>
      {dialog}
    </>
  );
}

afterEach(cleanup);

describe("AI翻訳の確認ダイアログ", () => {
  beforeEach(() => {
    useEditorStore.getState().loadEntries([entry], "project-1");
    useProjectStore.setState({
      details: {
        project: { id: "project-1" },
        namespaces: [{ namespace: "sample", containerPath: "" }],
        nestedArchives: [],
        mods: [{ modId: "sample", name: "Sample Mod" }],
      } as unknown as ProjectDetailsDto,
    });
    useAiJobStore.setState({
      activeJob: null,
      warnings: new Map(),
      notice: null,
      pendingEdits: [],
      baseline: new Map(),
      projectId: "project-1",
    });
    useSettingsStore.setState({
      settings: {
        schemaVersion: 1,
        ...SETTING_DEFAULTS,
        "ai.privacyNoticeAccepted": true,
        "editor.confirmBatchOverwrite": true,
      },
      loaded: true,
      error: null,
    });
    vi.mocked(getAiSettings).mockResolvedValue({
      provider: "openai",
      configured: true,
      model: "example-model",
      baseUrl: "https://api.openai.com/v1",
      batchSize: 50,
      concurrency: 2,
      customPrompt: "",
    });
    vi.mocked(saveSettings).mockResolvedValue();
    vi.mocked(startAiJob).mockResolvedValue("job-1");
  });

  it("上書き確認をDialogで表示し、キャンセル時はAIジョブを開始しない", async () => {
    const user = userEvent.setup();
    render(<ConfirmationHarness />);

    await user.click(screen.getByRole("button", { name: "AI翻訳" }));
    expect(await screen.findByRole("dialog", { name: "既存の翻訳を上書きしますか？" })).toBeTruthy();
    expect(screen.getByText("1件の翻訳が置き換わります。")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "キャンセル" }));

    await waitFor(() => expect(startAiJob).not.toHaveBeenCalled());
  });

  it("Dialogで上書きを選んだ場合だけAIジョブを開始する", async () => {
    const user = userEvent.setup();
    render(<ConfirmationHarness />);

    await user.click(screen.getByRole("button", { name: "AI翻訳" }));
    await user.click(await screen.findByRole("button", { name: "上書き" }));

    await waitFor(() => expect(startAiJob).toHaveBeenCalledOnce());
  });
});
