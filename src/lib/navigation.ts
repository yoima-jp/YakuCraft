type FlushHandler = () => Promise<boolean>;

let flushHandler: FlushHandler | null = null;

export function registerEditorFlush(handler: FlushHandler) {
  flushHandler = handler;
  return () => {
    if (flushHandler === handler) flushHandler = null;
  };
}

export async function flushBeforeLeavingEditor() {
  return flushHandler ? flushHandler() : true;
}
