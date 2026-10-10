import { createContext, useCallback, useContext, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";

import { DismissScopeContext, useDismiss } from "./useDismiss";

const MenuItemSelectionContext = createContext<() => void>(() => undefined);

interface MenuProps {
  "aria-label": string;
  children: ReactNode;
  className?: string;
  panelAlignment?: "start" | "end";
  panelClassName?: string;
  panelRole?: "group" | "menu";
  trigger: string;
}

export function Menu({
  "aria-label": ariaLabel,
  children,
  className,
  panelAlignment = "start",
  panelClassName,
  panelRole = "group",
  trigger,
}: MenuProps) {
  const id = useId();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const openedByKeyboardRef = useRef(false);
  const [open, setOpen] = useState(false);
  const closeAfterSelection = useCallback(() => {
    openedByKeyboardRef.current = false;
    setOpen(false);
    triggerRef.current?.focus();
  }, []);
  const dismissRefs = useMemo(() => [triggerRef, panelRef], []);
  const registerNestedRoot = useDismiss({
    open,
    refs: dismissRefs,
    onDismiss: (reason) => {
      // Escapeで閉じたときだけ呼び出し元へ戻し、外側をクリックした移動先のフォーカスは奪わない。
      if (reason === "escape") closeAfterSelection();
      else {
        openedByKeyboardRef.current = false;
        setOpen(false);
      }
    },
  });

  useLayoutEffect(() => {
    if (!open || panelRole !== "menu" || !openedByKeyboardRef.current) return;
    panelRef.current?.querySelector<HTMLElement>("[role=menuitem]:not(:disabled)")?.focus();
  }, [open, panelRole]);

  const toggle = useCallback(() => setOpen((current) => {
    if (current) openedByKeyboardRef.current = false;
    return !current;
  }), []);

  return (
    <DismissScopeContext.Provider value={registerNestedRoot}>
      <MenuItemSelectionContext.Provider value={closeAfterSelection}>
        <div className={`menu${className ? ` ${className}` : ""}`}>
          <button
            className="menu__trigger"
            type="button"
            aria-haspopup={panelRole === "menu" ? "menu" : "true"}
            aria-expanded={open}
            aria-controls={open ? id : undefined}
            aria-label={ariaLabel}
            ref={triggerRef}
            onClick={toggle}
            onMouseDown={() => { openedByKeyboardRef.current = false; }}
            onKeyDown={(event) => {
              if (!open && event.key === "ArrowDown") {
                // ArrowDownはbuttonのclickを起こさないため、開いて先頭項目へ移動する。
                event.preventDefault();
                openedByKeyboardRef.current = true;
                setOpen(true);
              } else if (!open && (event.key === "Enter" || event.key === " ")) {
                openedByKeyboardRef.current = true;
              }
            }}
          >
            <span>{trigger}</span>
            <span className="menu__icon" aria-hidden="true" />
          </button>
          {open && (
            <div
              className={`menu__panel${panelAlignment === "end" ? " menu__panel--end" : ""}${panelClassName ? ` ${panelClassName}` : ""}`}
              id={id}
              ref={panelRef}
              role={panelRole}
              aria-label={ariaLabel}
            >
              {children}
            </div>
          )}
        </div>
      </MenuItemSelectionContext.Provider>
    </DismissScopeContext.Provider>
  );
}

export function MenuItem({ className, onClick, onKeyDown, ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  const closeMenu = useContext(MenuItemSelectionContext);
  return (
    <button
      {...props}
      className={`menu__item${className ? ` ${className}` : ""}`}
      role="menuitem"
      tabIndex={-1}
      type="button"
      onClick={(event) => {
        onClick?.(event);
        closeMenu();
      }}
      onKeyDown={(event) => {
        onKeyDown?.(event);
        if (event.defaultPrevented || (event.key !== "ArrowDown" && event.key !== "ArrowUp")) return;
        const items = [...(event.currentTarget.closest('[role="menu"]')?.querySelectorAll<HTMLElement>(
          '[role="menuitem"]:not(:disabled):not([aria-disabled="true"])',
        ) ?? [])];
        if (items.length === 0) return;
        event.preventDefault();
        const currentIndex = items.indexOf(event.currentTarget);
        const step = event.key === "ArrowDown" ? 1 : -1;
        items[(currentIndex + step + items.length) % items.length].focus();
      }}
    />
  );
}
