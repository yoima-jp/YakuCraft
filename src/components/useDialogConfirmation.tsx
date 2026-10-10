import { useCallback, useEffect, useRef, useState } from "react";

import { PrimaryButton, SecondaryButton } from "./button/Buttons";
import { Dialog } from "./Controls";

export interface DialogConfirmationRequest {
  title: string;
  message: string;
  confirmLabel: string;
}

export type RequestConfirmation = (request: DialogConfirmationRequest) => Promise<boolean>;

export function useDialogConfirmation() {
  const [pending, setPending] = useState<DialogConfirmationRequest | null>(null);
  const resolveRef = useRef<((confirmed: boolean) => void) | null>(null);

  const settle = useCallback((confirmed: boolean) => {
    const resolve = resolveRef.current;
    resolveRef.current = null;
    setPending(null);
    resolve?.(confirmed);
  }, []);

  const requestConfirmation = useCallback<RequestConfirmation>((request) => new Promise((resolve) => {
    // 二重操作で確認が重なった場合、先の操作をキャンセル扱いにしてPromiseを残さない。
    resolveRef.current?.(false);
    resolveRef.current = resolve;
    setPending(request);
  }), []);

  useEffect(() => () => {
    resolveRef.current?.(false);
    resolveRef.current = null;
  }, []);

  const dialog = (
    <Dialog
      open={pending !== null}
      title={pending?.title ?? "確認"}
      onClose={() => settle(false)}
    >
      {pending && <p>{pending.message}</p>}
      <div className="dialog-confirmation__actions">
        <SecondaryButton onClick={() => settle(false)}>キャンセル</SecondaryButton>
        <PrimaryButton onClick={() => settle(true)}>{pending?.confirmLabel ?? "続ける"}</PrimaryButton>
      </div>
    </Dialog>
  );

  return { requestConfirmation, dialog };
}
