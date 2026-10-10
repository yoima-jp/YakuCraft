import { Children, createContext, useCallback, useContext, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ButtonHTMLAttributes, KeyboardEvent as ReactKeyboardEvent, MouseEvent as ReactMouseEvent, ReactNode } from "react";

import { useDismiss } from "./useDismiss";
import "./ContextMenu.css";

interface ContextMenuPosition {
  x: number;
  y: number;
  trigger: HTMLElement;
}

export interface ContextMenuTriggerProps {
  onContextMenu: (event: ReactMouseEvent<HTMLElement>) => void;
  onKeyDown: (event: ReactKeyboardEvent<HTMLElement>) => void;
}

export function useContextMenuTrigger() {
  const [position, setPosition] = useState<ContextMenuPosition | null>(null);

  const open = useCallback((x: number, y: number, trigger: HTMLElement) => {
    setPosition({ x, y, trigger });
  }, []);

  const triggerProps = useMemo<ContextMenuTriggerProps>(() => ({
    onContextMenu: (event) => {
      if (shouldPreserveNativeContextMenu(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      open(event.clientX, event.clientY, focusTarget(event.target, event.currentTarget));
    },
    onKeyDown: (event) => {
      if (!((event.shiftKey && event.key === "F10") || event.key === "ContextMenu")) return;
      if (shouldPreserveNativeContextMenu(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      const bounds = event.currentTarget.getBoundingClientRect();
      open(bounds.left, bounds.bottom, focusTarget(event.target, event.currentTarget));
    },
  }), [open]);

  return { position, triggerProps, close: useCallback(() => setPosition(null), []) };
}

interface ContextMenuProps {
  label: string;
  position: ContextMenuPosition;
  children: ReactNode;
  onClose: () => void;
}

interface MenuActions {
  close: () => void;
}

const MenuActionsContext = createContext<MenuActions>({ close: () => undefined });

export function ContextMenu({ label, position, children, onClose }: ContextMenuProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLElement | null>(position.trigger);
  const dismissRefs = useMemo(() => [triggerRef, panelRef], []);
  const closeAndRestore = useCallback((restoreFocus: boolean) => {
    onClose();
    const trigger = triggerRef.current;
    if (restoreFocus && trigger?.isConnected) trigger.focus({ preventScroll: true });
  }, [onClose]);

  useDismiss({
    open: true,
    refs: dismissRefs,
    onDismiss: (reason) => closeAndRestore(reason === "escape"),
  });

  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    const bounds = panel.getBoundingClientRect();
    const gutter = 8;
    const width = document.documentElement.clientWidth || window.innerWidth;
    const height = document.documentElement.clientHeight || window.innerHeight;
    panel.style.left = `${Math.max(gutter, Math.min(position.x, width - bounds.width - gutter))}px`;
    panel.style.top = `${Math.max(gutter, Math.min(position.y, height - bounds.height - gutter))}px`;
    panel.querySelector<HTMLElement>('[role="menuitem"]:not(:disabled):not([aria-disabled="true"])')?.focus();
  }, [position]);

  const moveFocus = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = [...(event.currentTarget.querySelectorAll<HTMLElement>(
      '[role="menuitem"]:not(:disabled):not([aria-disabled="true"])',
    ))];
    if (items.length === 0) return;
    event.preventDefault();
    const currentIndex = items.indexOf(document.activeElement as HTMLElement);
    const index = event.key === "Home"
      ? 0
      : event.key === "End"
        ? items.length - 1
        : (currentIndex + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
    items[index]?.focus();
  };

  return createPortal(
    <MenuActionsContext.Provider value={{ close: () => closeAndRestore(true) }}>
      <div
        className="context-menu"
        ref={panelRef}
        role="menu"
        tabIndex={-1}
        aria-label={label}
        onKeyDown={moveFocus}
        style={{ left: position.x, top: position.y }}
      >
        {children}
      </div>
    </MenuActionsContext.Provider>,
    document.body,
  );
}

export function ContextMenuItem({ className, onClick, children, ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  const { close } = useContext(MenuActionsContext);
  const labelId = useId();
  const [label, ...suffix] = Children.toArray(children);
  return (
    <button
      {...props}
      className={`context-menu__item${className ? ` ${className}` : ""}`}
      aria-labelledby={labelId}
      type="button"
      role="menuitem"
      tabIndex={-1}
      onClick={(event) => {
        onClick?.(event);
        close();
      }}
    >
      <span id={labelId}>{label}</span>
      {suffix}
    </button>
  );
}

export function ContextMenuSeparator() {
  return <hr className="context-menu__separator" />;
}

export function ContextMenuLabel({ children }: { children: ReactNode }) {
  return <div className="context-menu__label" role="presentation">{children}</div>;
}

export function ContextMenuShortcut({ children }: { children: ReactNode }) {
  return <span className="context-menu__shortcut" aria-hidden="true">{children}</span>;
}

export function useContextMenuPolicy() {
  useLayoutEffect(() => {
    const preserveNativeMenu = (event: MouseEvent) => {
      if (!shouldPreserveNativeContextMenu(event.target)) event.preventDefault();
    };
    document.addEventListener("contextmenu", preserveNativeMenu, true);
    return () => document.removeEventListener("contextmenu", preserveNativeMenu, true);
  }, []);
}

export function shouldPreserveNativeContextMenu(target: EventTarget | null) {
  if (!(target instanceof Element)) return false;
  if (target.closest("input, textarea, select, [contenteditable='true'], [contenteditable=''], .selectable, .analyze-screen__error pre")) return true;

  // Keep the platform copy menu for selected text in explicitly selectable diagnostics and paths.
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return false;
  const range = selection.getRangeAt(0);
  if (!range.intersectsNode(target)) return false;
  let element: Element | null = target;
  while (element) {
    if (getComputedStyle(element).userSelect === "text") return true;
    element = element.parentElement;
  }
  return false;
}

function focusTarget(target: EventTarget | null, fallback: HTMLElement) {
  if (target instanceof HTMLElement && target.matches("button, input, textarea, select, [tabindex]")) return target;
  if (target instanceof Element) {
    const ancestor = target.closest<HTMLElement>("button, input, textarea, select, [tabindex]");
    if (ancestor) return ancestor;
  }
  return fallback;
}
