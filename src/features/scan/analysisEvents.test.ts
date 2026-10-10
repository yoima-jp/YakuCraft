import { describe, expect, it } from "vitest";

import type { AnalysisChannelEvent } from "../../types/commands";
import { isAnalysisEventForJob } from "./analysisEvents";

const progressEvent: AnalysisChannelEvent = {
  kind: "progress",
  jobId: "old-job",
  progress: { stage: "parseLanguages", current: 1, total: 2, message: "解析中" },
  modIndex: 1,
  modCount: 1,
  modFilename: "sample.jar",
};

describe("isAnalysisEventForJob", () => {
  it("accepts only progress and terminal events for the active job", () => {
    expect(isAnalysisEventForJob(progressEvent, "old-job", "old-job")).toBe(true);
    expect(isAnalysisEventForJob(progressEvent, "old-job", "new-job")).toBe(false);
    expect(isAnalysisEventForJob(progressEvent, "new-job", "new-job")).toBe(false);
    expect(isAnalysisEventForJob(progressEvent, null, null)).toBe(false);

    expect(isAnalysisEventForJob({
      kind: "failed",
      jobId: "old-job",
      error: { code: "failed", userMessage: "失敗", recoverable: true },
    }, "old-job", "old-job")).toBe(true);
    expect(isAnalysisEventForJob({
      kind: "completed",
      completed: { jobId: "old-job", projectId: "project", failedMods: [] },
    }, "old-job", "old-job")).toBe(true);
    expect(isAnalysisEventForJob({
      kind: "completed",
      completed: { jobId: "old-job", projectId: "project", failedMods: [] },
    }, "old-job", "new-job")).toBe(false);
  });
});
