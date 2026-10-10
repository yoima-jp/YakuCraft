import type { AnalysisChannelEvent } from "../../types/commands";

export function isAnalysisEventForJob(
  event: AnalysisChannelEvent,
  expectedJobId: string | null,
  activeJobId: string | null,
): boolean {
  // ScanScreenを離れた後もChannel callbackが残るため、個別IDと現在の共有IDを両方照合する。
  if (expectedJobId === null || expectedJobId !== activeJobId) return false;
  const eventJobId = event.kind === "completed" ? event.completed.jobId : event.jobId;
  return eventJobId === expectedJobId;
}
