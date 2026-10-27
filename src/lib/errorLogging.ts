import { invoke } from "@tauri-apps/api/core";

const MAX_ERROR_BYTES = 4 * 1024;

function redactSensitiveLines(value: string) {
  return value.split("\n").map((line) => {
    if (/authorization|api[ _-]?key|credential|password|secret|bearer\s|token|\b(?:sk[-_]|AIza|gsk_|xai-|pplx-|hf_)/i.test(line)) {
      return "[redacted sensitive error content]";
    }
    return line;
  }).join("\n");
}

function isAppError(value: Record<string, unknown>) {
  return typeof value.code === "string"
    && typeof value.userMessage === "string"
    && typeof value.recoverable === "boolean";
}

function isAlreadyLogged(value: unknown): boolean {
  if (typeof value !== "object" || value === null) return false;
  const error = value as Record<string, unknown>;
  return isAppError(error)
    || (typeof error.appError === "object" && error.appError !== null)
    || error.name === "ExportCommandError";
}

function errorDetails(error: unknown, context: string) {
  const lines = [`context: ${context}`];
  if (error instanceof Error) {
    lines.push(`message: ${error.message}`);
    if (error.stack) lines.push(`stack:\n${error.stack.split("\n").slice(0, 5).join("\n")}`);
  } else if (typeof error === "string") {
    lines.push(`message: ${error}`);
  } else if (typeof error === "object" && error !== null) {
    const value = error as Record<string, unknown>;
    if (typeof value.code === "string") lines.push(`code: ${value.code}`);
    if (typeof value.message === "string") lines.push(`message: ${value.message}`);
    if (typeof value.detail === "string") lines.push(`detail: ${value.detail}`);
    if (typeof value.stack === "string") {
      lines.push(`stack:\n${value.stack.split("\n").slice(0, 5).join("\n")}`);
    }
  } else {
    lines.push(`message: ${String(error)}`);
  }
  return redactSensitiveLines(lines.join("\n"));
}

function truncateUtf8(value: string, maxBytes: number) {
  const bytes = new TextEncoder().encode(value);
  if (bytes.length <= maxBytes) return value;
  let end = maxBytes - new TextEncoder().encode("…").length;
  while (end > 0 && (bytes[end] & 0b1100_0000) === 0b1000_0000) end -= 1;
  return `${new TextDecoder().decode(bytes.subarray(0, end))}…`;
}

export async function logFrontendError(error: unknown, context: string) {
  if (isAlreadyLogged(error)) return;

  try {
    await invoke<void>("log_frontend_error", {
      error: truncateUtf8(errorDetails(error, context), MAX_ERROR_BYTES),
    });
  } catch {
    // エラー送信に失敗しても、元の画面処理を続ける。
  }
}
