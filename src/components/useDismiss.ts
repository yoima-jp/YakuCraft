import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useRef } from "react";
import type { RefObject } from "react";
import { isImeCompositionKeyDown } from "../lib/keyboard";

export type DismissReason = "outside-pointer" | "escape" | "focus-out" | "resize" | "scroll";

type DismissRef = Readonly<{ current: Element | null }>;
type RegisterDismissRoot = (ref: DismissRef) => () => void;

export const DismissScopeContext = createContext<RegisterDismissRoot | null>(null);

interface UseDismissOptions {
  open: boolean;
  onDismiss: (reason: DismissReason) => void;
  refs: readonly RefObject<Element | null>[];
  dismissOnResize?: boolean;
  dismissOnScroll?: boolean;
}

export function useDismiss({
  open,
  onDismiss,
  refs,
  dismissOnResize = true,
  dismissOnScroll = true,
}: UseDismissOptions): RegisterDismissRoot {
  const parentRegisterRoot = useContext(DismissScopeContext);
  const refsRef = useRef(refs);
  const nestedRootsRef = useRef(new Set<DismissRef>());
  const onDismissRef = useRef(onDismiss);

  useEffect(() => { refsRef.current = refs; }, [refs]);
  useEffect(() => { onDismissRef.current = onDismiss; }, [onDismiss]);

  const registerNestedRoot = useCallback<RegisterDismissRoot>((ref) => {
    // bodyへPortalする子ポップアップも親の内側として扱い、選択操作で親まで閉じないようにする。
    nestedRootsRef.current.add(ref);
    return () => nestedRootsRef.current.delete(ref);
  }, []);

  const popupRef = refs[1];
  // Register a portal as part of the layout phase so a child popup is inside the
  // parent's dismiss scope before the child moves focus into its portal.
  useLayoutEffect(() => {
    if (!open || !parentRegisterRoot || !popupRef) return;
    return parentRegisterRoot(popupRef);
  }, [open, parentRegisterRoot, popupRef]);

  useEffect(() => {
    if (!open) return;

    const contains = (target: Node) => [
      ...refsRef.current,
      ...nestedRootsRef.current,
    ].some((ref) => ref.current?.contains(target));
    const containsNestedRoot = (target: Node) => [...nestedRootsRef.current]
      .some((ref) => ref.current?.contains(target));

    const dismiss = (reason: DismissReason) => onDismissRef.current(reason);
    const handlePointerDown = (event: PointerEvent) => {
      if (!(event.target instanceof Node) || contains(event.target)) return;
      dismiss("outside-pointer");
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (isImeCompositionKeyDown(event)) return;
      if (event.key !== "Escape" || event.defaultPrevented || !(event.target instanceof Node)) return;
      // 子ポップアップ内のEscapeは子が処理し、親は閉じない。
      if (containsNestedRoot(event.target)) return;
      if (!contains(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      dismiss("escape");
    };
    const handleFocusIn = (event: FocusEvent) => {
      if (!(event.target instanceof Node) || contains(event.target)) return;
      dismiss("focus-out");
    };
    const handleResize = () => {
      if (dismissOnResize) dismiss("resize");
    };
    const handleScroll = (event: Event) => {
      if (!dismissOnScroll) return;
      if (event.target instanceof Node && contains(event.target)) return;
      dismiss("scroll");
    };

    // 外側クリックは列幅ハンドルなどが伝播を止めても検知できるよう、キャプチャ段階で判定する。
    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown);
    document.addEventListener("focusin", handleFocusIn);
    document.addEventListener("scroll", handleScroll, true);
    window.addEventListener("resize", handleResize);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown);
      document.removeEventListener("focusin", handleFocusIn);
      document.removeEventListener("scroll", handleScroll, true);
      window.removeEventListener("resize", handleResize);
    };
  }, [dismissOnResize, dismissOnScroll, open]);

  return registerNestedRoot;
}
