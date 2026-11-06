import { useCallback, useEffect, useRef, useState } from "react";

import { DestructiveButton, PrimaryButton, SecondaryButton } from "./button/Buttons";
import { Dialog } from "./Controls";
import { useT } from "../i18n";

export interface DialogConfirmationRequest {
  title: string;
  message: string;
  confirmLabel: string;
  destructive?: boolean;
}

export type RequestConfirmation = (request: DialogConfirmationRequest) => Promise<boolean>;

export function useDialogConfirmation() {
  const translate = useT();
  // Tauri の dialog プラグインはブラウザー標準の確認APIを非同期に差し替えるため、同期booleanとして扱わず、この共有ダイアログでPromiseを待つ。
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
      title={pending?.title ?? translate("common.confirm")}
      onClose={() => settle(false)}
    >
      {pending && <p className="dialog-confirmation__message">{pending.message}</p>}
      <div className="dialog-confirmation__actions">
        <SecondaryButton onClick={() => settle(false)}>{translate("common.cancel")}</SecondaryButton>
        {pending?.destructive
          ? <DestructiveButton onClick={() => settle(true)}>{pending.confirmLabel}</DestructiveButton>
          : <PrimaryButton onClick={() => settle(true)}>{pending?.confirmLabel ?? translate("common.continue")}</PrimaryButton>}
      </div>
    </Dialog>
  );

  return { requestConfirmation, dialog };
}
