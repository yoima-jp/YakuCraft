import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type InputHTMLAttributes,
  type FocusEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

import { useDismiss } from "./useDismiss";
import { isImeCompositionKeyDown } from "../lib/keyboard";
import "./form-controls.css";
export { Tooltip } from "./Tooltip";
export { Menu, MenuItem } from "./Menu";

/* oxlint-disable jsx-a11y/prefer-tag-over-role -- ブラウザー既定の表示を避けながら、独自ポップアップとダイアログに必要なARIA roleを保つ。 */

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

interface SelectProps {
  "aria-label": string;
  className?: string;
  disabled?: boolean;
  options: readonly SelectOption[];
  placeholder?: string;
  value: string;
  onChange: (value: string) => void;
  onBlur?: (event: FocusEvent<HTMLButtonElement>) => void;
}

interface PopupPosition {
  top: number;
  left: number;
  width: number;
  maxHeight: number;
  placement: "above" | "below";
}

function nextEnabledIndex(options: readonly SelectOption[], from: number, direction: 1 | -1) {
  for (let offset = 1; offset <= options.length; offset += 1) {
    const index = (from + direction * offset + options.length * 2) % options.length;
    if (!options[index].disabled) return index;
  }
  return -1;
}

export function Select({
  "aria-label": ariaLabel,
  className,
  disabled = false,
  options,
  placeholder = "選択",
  value,
  onChange,
  onBlur,
}: SelectProps) {
  const id = useId();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const typeaheadRef = useRef("");
  const typeaheadTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const [position, setPosition] = useState<PopupPosition>({
    top: 8,
    left: 8,
    width: 220,
    maxHeight: 240,
    placement: "below",
  });
  const dismissRefs = useMemo(() => [triggerRef, menuRef], []);
  useDismiss({
    open,
    refs: dismissRefs,
    onDismiss: (reason) => {
      setOpen(false);
      if (reason === "escape") triggerRef.current?.focus();
    },
  });
  const selectedIndex = options.findIndex((option) => option.value === value);
  const selectedOption = options[selectedIndex];

  const openAt = (index: number) => {
    setActiveIndex(index);
    setOpen(true);
  };

  const choose = (index: number) => {
    const option = options[index];
    if (!option || option.disabled) return;
    onChange(option.value);
    setOpen(false);
    triggerRef.current?.focus();
  };

  const moveActive = (direction: 1 | -1) => {
    const index = nextEnabledIndex(options, activeIndex, direction);
    if (index >= 0) setActiveIndex(index);
  };

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      if (!open) {
        const start = selectedIndex >= 0 ? selectedIndex : (direction === 1 ? -1 : 0);
        const index = direction === 1 && selectedIndex < 0
          ? options.findIndex((option) => !option.disabled)
          : direction === -1 && selectedIndex < 0
            ? nextEnabledIndex(options, 0, -1)
            : options[start]?.disabled ? nextEnabledIndex(options, start, direction) : start;
        openAt(index);
      } else {
        moveActive(direction);
      }
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      let lastEnabledIndex = options.length - 1;
      while (lastEnabledIndex >= 0 && options[lastEnabledIndex].disabled) lastEnabledIndex -= 1;
      const index = event.key === "Home" ? options.findIndex((option) => !option.disabled) : lastEnabledIndex;
      if (open) setActiveIndex(index);
      else openAt(index);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (open) choose(activeIndex);
      else openAt(selectedIndex >= 0 ? selectedIndex : options.findIndex((option) => !option.disabled));
      return;
    }
    if (event.key === "Escape" && open) {
      // Select内のEscは候補だけを閉じ、親Popoverは次のEscまで開いたままにする。
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
      return;
    }
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      const query = `${typeaheadRef.current}${event.key}`.toLocaleLowerCase();
      typeaheadRef.current = query;
      if (typeaheadTimerRef.current) clearTimeout(typeaheadTimerRef.current);
      typeaheadTimerRef.current = setTimeout(() => { typeaheadRef.current = ""; }, 700);
      const start = open ? activeIndex : selectedIndex;
      let match = -1;
      for (let offset = 1; offset <= options.length; offset += 1) {
        const index = (start + offset + options.length) % options.length;
        if (!options[index].disabled && options[index].label.toLocaleLowerCase().startsWith(query)) {
          match = index;
          break;
        }
      }
      if (match >= 0) {
        event.preventDefault();
        setActiveIndex(match);
        setOpen(true);
      }
    }
  };

  // 候補が画面端で切れないよう、開いた時点の表示領域から位置を決める。
  useLayoutEffect(() => {
    if (!open) return;
    const updatePosition = () => {
      const trigger = triggerRef.current;
      const menu = menuRef.current;
      if (!trigger || !menu) return;
      const rect = trigger.getBoundingClientRect();
      const viewportPadding = 8;
      const below = Math.max(0, window.innerHeight - rect.bottom - viewportPadding);
      const above = Math.max(0, rect.top - viewportPadding);
      const contentHeight = Math.min(menu.scrollHeight, 288);
      const placement = below >= contentHeight || below >= above ? "below" : "above";
      const available = placement === "below" ? below : above;
      const maxHeight = Math.max(48, Math.min(288, available));
      const width = Math.min(
        Math.max(rect.width, menu.scrollWidth),
        Math.max(0, window.innerWidth - viewportPadding * 2),
      );
      const left = Math.min(
        Math.max(viewportPadding, rect.left),
        Math.max(viewportPadding, window.innerWidth - width - viewportPadding),
      );
      const height = Math.min(menu.scrollHeight, maxHeight);
      const top = placement === "below"
        ? Math.min(rect.bottom + 4, window.innerHeight - height - viewportPadding)
        : Math.max(viewportPadding, rect.top - height - 4);
      setPosition({ top, left, width, maxHeight, placement });
    };
    updatePosition();
  }, [open, options.length]);

  useEffect(() => {
    if (activeIndex < 0 || !open) return;
    menuRef.current?.querySelectorAll<HTMLElement>("[role=option]")[activeIndex]?.scrollIntoView?.({ block: "nearest" });
  }, [activeIndex, id, open]);

  useEffect(() => () => {
    if (typeaheadTimerRef.current) clearTimeout(typeaheadTimerRef.current);
  }, []);

  // フォーカスはcomboboxに保ち、ARIAのactive descendantで候補位置を伝えるとTab順が安定する。
  const menu = open && typeof document !== "undefined"
    ? createPortal(
      <div
        className="select__menu"
        id={`${id}-listbox`}
        ref={menuRef}
        role="listbox"
        aria-label={ariaLabel}
        data-placement={position.placement}
        style={{
          top: position.top,
          left: position.left,
          width: position.width,
          maxHeight: position.maxHeight,
        }}
      >
        {options.map((option, index) => (
          <div
            className="select__option"
            id={`${id}-option-${index}`}
            key={option.value}
            role="option"
            data-value={option.value}
            aria-selected={option.value === value}
            aria-disabled={option.disabled || undefined}
            data-active={index === activeIndex || undefined}
            tabIndex={-1}
            onMouseEnter={() => { if (!option.disabled) setActiveIndex(index); }}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => choose(index)}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                choose(index);
              }
            }}
          >
            {option.label}
          </div>
        ))}
      </div>,
      document.body,
    )
    : null;

  return (
    <span className={`select${className ? ` ${className}` : ""}`}>
      <button
        className="select__trigger"
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? `${id}-listbox` : undefined}
        aria-activedescendant={open && activeIndex >= 0 ? `${id}-option-${activeIndex}` : undefined}
        disabled={disabled}
        ref={triggerRef}
        onClick={() => {
          if (open) setOpen(false);
          else openAt(selectedIndex >= 0 ? selectedIndex : options.findIndex((option) => !option.disabled));
        }}
        onBlur={onBlur}
        onKeyDown={handleKeyDown}
      >
        <span>{selectedOption?.label ?? placeholder}</span>
      </button>
      {menu}
    </span>
  );
}

interface ComboboxProps {
  id: string;
  value: string;
  options: readonly string[];
  disabled?: boolean;
  "aria-describedby"?: string;
  onChange: (value: string) => void;
}

export function Combobox({ id, value, options, disabled = false, "aria-describedby": describedBy, onChange }: ComboboxProps) {
  const listId = useId();
  const rootRef = useRef<HTMLSpanElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const [position, setPosition] = useState<PopupPosition>({ top: 8, left: 8, width: 220, maxHeight: 240, placement: "below" });
  const dismissRefs = useMemo(() => [rootRef, menuRef], []);
  useDismiss({ open, refs: dismissRefs, onDismiss: () => setOpen(false) });

  // 未取得や独自モデルも入力できるようにし、候補は消さず一致順に並べる。
  const sortedOptions = useMemo(() => {
    const query = value.toLocaleLowerCase();
    const rank = (option: string) => {
      const text = option.toLocaleLowerCase();
      if (!query) return 0;
      if (text === query) return 0;
      if (text.startsWith(query)) return 1;
      if (text.includes(query)) return 2;
      return 3;
    };
    return options.map((option, index) => ({ option, index, rank: rank(option) }))
      .sort((a, b) => a.rank - b.rank || a.index - b.index)
      .map(({ option }) => option);
  }, [options, value]);
  const visible = open && options.length > 0 && !disabled;
  const choose = (index: number) => {
    const option = sortedOptions[index];
    if (option === undefined) return;
    onChange(option);
    setOpen(false);
    inputRef.current?.focus();
  };

  useLayoutEffect(() => {
    if (!visible) return;
    const trigger = rootRef.current;
    const menu = menuRef.current;
    if (!trigger || !menu) return;
    const rect = trigger.getBoundingClientRect();
    const padding = 8;
    const below = Math.max(0, window.innerHeight - rect.bottom - padding);
    const above = Math.max(0, rect.top - padding);
    const placement = below >= Math.min(menu.scrollHeight, 288) || below >= above ? "below" : "above";
    const maxHeight = Math.max(48, Math.min(288, placement === "below" ? below : above));
    const width = Math.min(Math.max(rect.width, menu.scrollWidth), Math.max(0, window.innerWidth - padding * 2));
    const left = Math.min(Math.max(padding, rect.left), Math.max(padding, window.innerWidth - width - padding));
    const height = Math.min(menu.scrollHeight, maxHeight);
    const top = placement === "below" ? Math.min(rect.bottom + 4, window.innerHeight - height - padding) : Math.max(padding, rect.top - height - 4);
    setPosition({ top, left, width, maxHeight, placement });
  }, [visible, options.length]);

  useEffect(() => {
    if (visible && activeIndex >= 0) menuRef.current?.querySelectorAll<HTMLElement>("[role=option]")[activeIndex]?.scrollIntoView?.({ block: "nearest" });
  }, [visible, activeIndex]);

  return (
    <span className="combobox" ref={rootRef}>
      <input
        id={id} ref={inputRef} value={value} disabled={disabled} role={options.length ? "combobox" : undefined}
        aria-expanded={options.length ? visible : undefined} aria-controls={visible ? `${listId}-listbox` : undefined}
        aria-activedescendant={visible && activeIndex >= 0 ? `${listId}-option-${activeIndex}` : undefined}
        aria-autocomplete={options.length ? "list" : undefined} aria-describedby={describedBy}
        onFocus={() => { if (options.length) setOpen(true); }}
        onClick={() => { if (options.length) setOpen(true); }}
        onChange={(event) => { setActiveIndex(-1); onChange(event.currentTarget.value); if (options.length) setOpen(true); }}
        onKeyDown={(event) => {
          if (isImeCompositionKeyDown(event.nativeEvent)) return;
          if ((event.key === "ArrowDown" || event.key === "ArrowUp") && options.length) {
            event.preventDefault();
            const direction = event.key === "ArrowDown" ? 1 : -1;
            setActiveIndex((current) => current < 0 ? (direction === 1 ? 0 : sortedOptions.length - 1) : (current + direction + sortedOptions.length) % sortedOptions.length);
            setOpen(true);
          } else if (event.key === "Enter" && visible && activeIndex >= 0) {
            event.preventDefault(); choose(activeIndex);
          } else if (event.key === "Escape" && visible) {
            event.preventDefault(); event.stopPropagation(); setOpen(false);
          } else if (event.key === "Tab") setOpen(false);
        }}
      />
      {options.length > 0 && <button type="button" className="combobox__toggle" aria-label="候補を表示" tabIndex={-1} disabled={disabled}
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => { inputRef.current?.focus(); setOpen(!visible); setActiveIndex(-1); }}><span className="menu__icon" aria-hidden="true" /></button>}
      {visible && typeof document !== "undefined" && createPortal(
        <div className="select__menu" id={`${listId}-listbox`} ref={menuRef} role="listbox" aria-label="モデル候補"
          data-placement={position.placement} style={{ top: position.top, left: position.left, width: position.width, maxHeight: position.maxHeight }}>
          {sortedOptions.map((option, index) => <div className="select__option" id={`${listId}-option-${index}`} key={option} role="option"
            aria-selected={option === value} data-active={index === activeIndex || undefined} tabIndex={-1}
            onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); choose(index); } }}
            onMouseEnter={() => setActiveIndex(index)} onMouseDown={(event) => event.preventDefault()} onClick={() => choose(index)}>{option}</div>)}
        </div>, document.body)}
    </span>
  );
}

interface CheckboxProps {
  checked: boolean;
  className?: string;
  disabled?: boolean;
  id?: string;
  name?: string;
  children: ReactNode;
  onChange: (checked: boolean) => void;
}

export function Checkbox({ checked, className, disabled = false, id, name, children, onChange }: CheckboxProps) {
  return (
    <label className={`checkbox${className ? ` ${className}` : ""}`}>
      {/* inputをdisplay:noneにするとSpace操作と支援技術の状態通知を失うため、視覚だけ隠して残す。 */}
      <input
        className="checkbox__input"
        id={id}
        name={name}
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.currentTarget.checked)}
      />
      <span className="checkbox__indicator" aria-hidden="true" />
      <span className="checkbox__text">{children}</span>
    </label>
  );
}

export interface RadioOption {
  value: string;
  label: string;
  description?: ReactNode;
  disabled?: boolean;
  className?: string;
  id?: string;
  testId?: string;
}

interface RadioGroupProps {
  className?: string;
  legend: string;
  legendClassName?: string;
  name: string;
  optionClassName?: string;
  options: readonly RadioOption[];
  value: string;
  onChange: (value: string) => void;
}

export function RadioGroup({
  className,
  legend,
  legendClassName,
  name,
  optionClassName,
  options,
  value,
  onChange,
}: RadioGroupProps) {
  return (
    <fieldset className={`radio-group${className ? ` ${className}` : ""}`}>
      <legend className={legendClassName ?? "visually-hidden"}>{legend}</legend>
      <div className="radio-group__options">
        {options.map((option) => (
          <label
            className={`radio-group__option${optionClassName ? ` ${optionClassName}` : ""}${option.className ? ` ${option.className}` : ""}`}
            htmlFor={option.id}
            key={option.value}
          >
            <input
              className="radio-group__input"
              id={option.id}
              data-testid={option.testId}
              name={name}
              type="radio"
              value={option.value}
              checked={value === option.value}
              disabled={option.disabled}
              onChange={() => onChange(option.value)}
            />
            <span className="radio-group__indicator" aria-hidden="true" />
            <span className="radio-group__content">
              <strong>{option.label}</strong>
              {option.description && <span>{option.description}</span>}
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}

type NumberInputProps = Omit<InputHTMLAttributes<HTMLInputElement>, "type">;

export function NumberInput({ className, ...props }: NumberInputProps) {
  return <input {...props} className={`number-input${className ? ` ${className}` : ""}`} type="number" />;
}

interface DisclosureProps {
  children: ReactNode;
  className?: string;
  defaultOpen?: boolean;
  title: string;
}

export function Disclosure({ children, className, defaultOpen = false, title }: DisclosureProps) {
  const id = useId();
  const [open, setOpen] = useState(defaultOpen);

  return (
    <section className={`disclosure${className ? ` ${className}` : ""}`}>
      <button
        className="disclosure__trigger"
        type="button"
        aria-expanded={open}
        aria-controls={`${id}-content`}
        onClick={() => setOpen((current) => !current)}
      >
        <span>{title}</span>
        <span className="disclosure__icon" aria-hidden="true" />
      </button>
      <div className="disclosure__content" id={`${id}-content`} hidden={!open}>
        {children}
      </div>
    </section>
  );
}

interface DialogProps {
  children: ReactNode;
  closeOnBackdrop?: boolean;
  onClose: () => void;
  open: boolean;
  title: string;
}

export function Dialog({ children, closeOnBackdrop = false, onClose, open, title }: DialogProps) {
  const id = useId();
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  useEffect(() => { onCloseRef.current = onClose; }, [onClose]);

  useLayoutEffect(() => {
    if (!open) return;
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const dialog = dialogRef.current;
    // モーダル中のTabを内側に留め、閉じたときは呼び出し元へフォーカスを戻す。
    const focusableSelector = "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex='-1'])";
    const firstFocusable = dialog?.querySelector<HTMLElement>("[autofocus]")
      ?? dialog?.querySelector<HTMLElement>(focusableSelector);
    (firstFocusable ?? dialog)?.focus();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (isImeCompositionKeyDown(event)) return;
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
        return;
      }
      if (event.key !== "Tab" || !dialog) return;
      const focusable = [...dialog.querySelectorAll<HTMLElement>(focusableSelector)];
      if (focusable.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (document.activeElement === last || !dialog.contains(document.activeElement))) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, [open]);

  if (!open || typeof document === "undefined") return null;
  return createPortal(
    // oxlint-disable-next-line jsx-a11y/click-events-have-key-events, jsx-a11y/no-static-element-interactions -- 背景は任意のクリックで閉じ、キーボード操作にはEscと明示ボタンを使う。
    <div
      className="dialog-backdrop"
      onClick={(event) => {
        if (closeOnBackdrop && event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="dialog"
        id={`${id}-dialog`}
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${id}-title`}
        tabIndex={-1}
      >
        <h2 className="dialog__title" id={`${id}-title`}>{title}</h2>
        <div className="dialog__body">{children}</div>
      </div>
    </div>,
    document.body,
  );
}
