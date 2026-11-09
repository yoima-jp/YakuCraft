import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { compareText, lowerText, useT, useUiLanguage } from "../i18n";
import { findMinecraftLanguage, MINECRAFT_LANGUAGES, minecraftLanguageLabel, targetLanguageName } from "../i18n/minecraftLanguages";
import "./TargetLanguagePicker.css";

/* oxlint-disable jsx-a11y/prefer-tag-over-role, jsx-a11y/no-noninteractive-element-to-interactive-role -- 検索候補はactive descendantによるキーボード操作のためARIA roleで表す。 */

interface TargetLanguagePickerProps {
  "aria-label": string;
  className?: string;
  disabled?: boolean;
  id?: string;
  value: string;
  onChange: (value: string) => void;
  // 選択できない言語も一覧には残す。隠すと、なぜその言語が無いのか分からなくなるため。
  isOptionDisabled?: (code: string) => boolean;
  disabledOptionReason?: string;
}

export function TargetLanguagePicker({
  "aria-label": ariaLabel,
  className,
  disabled = false,
  id: givenId,
  value,
  onChange,
  isOptionDisabled,
  disabledOptionReason,
}: TargetLanguagePickerProps) {
  const generatedId = useId();
  const id = givenId ?? `target-language-${generatedId}`;
  const t = useT();
  const uiLanguage = useUiLanguage();
  const listRef = useRef<HTMLUListElement>(null);
  // 文字入力欄はマウスで押しても :focus-visible に一致するため、そのままだと押しただけで緑のリングが出る。
  // この欄は見た目も役割もセレクトと同じなので、ボタン製の Select と同じくマウス操作ではリングを出さず、
  // Tab で入ったときだけ出す。pointerdown は focus より先に届くので、その印を focus で受け取って判定する。
  const pointerDownRef = useRef(false);
  const [pointerFocused, setPointerFocused] = useState(false);
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const selected = findMinecraftLanguage(value);
  const selectedLabel = selected ? `${minecraftLanguageLabel(value)} · ${value}` : value;
  const localizedLanguageNames = useMemo(() => new Map(
    MINECRAFT_LANGUAGES.map((language) => [language.code, targetLanguageName(language.code, uiLanguage)]),
  ), [uiLanguage]);
  const options = useMemo(() => {
    const normalizedQuery = lowerText(query.trim(), uiLanguage);
    if (!normalizedQuery) return MINECRAFT_LANGUAGES;
    return MINECRAFT_LANGUAGES.filter((language) => [
      language.name,
      language.region,
      language.code,
      localizedLanguageNames.get(language.code) ?? language.name,
    ].some((part) => lowerText(part, uiLanguage).includes(normalizedQuery)))
      .sort((left, right) => compareText(left.name, right.name, uiLanguage));
  }, [localizedLanguageNames, query, uiLanguage]);
  const activeOptionIndex = options.length > 0 ? Math.min(activeIndex, options.length - 1) : -1;
  const isDisabled = (code: string) => isOptionDisabled?.(code) === true;
  const firstEnabledIndex = () => options.findIndex((language) => !isDisabled(language.code));
  // 矢印キーは無効な候補を飛ばし、全候補が無効なら現在位置を維持する。
  const nextEnabledIndex = (from: number, direction: 1 | -1) => {
    for (let offset = 1; offset <= options.length; offset += 1) {
      const index = (from + direction * offset + options.length * 2) % options.length;
      if (!isDisabled(options[index].code)) return index;
    }
    return -1;
  };

  useEffect(() => {
    if (open && activeOptionIndex >= 0) {
      listRef.current?.querySelectorAll<HTMLElement>("[role=option]")[activeOptionIndex]?.scrollIntoView?.({ block: "nearest" });
    }
  }, [activeOptionIndex, open]);

  const choose = (code: string) => {
    if (disabled || isDisabled(code)) return;
    onChange(code);
    setQuery("");
    setOpen(false);
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (disabled) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      if (!open) {
        setOpen(true);
        setActiveIndex(firstEnabledIndex());
      } else if (options.length > 0) {
        const next = nextEnabledIndex(activeOptionIndex, direction);
        if (next >= 0) setActiveIndex(next);
      }
    } else if (event.key === "Home" && open && options.length > 0) {
      event.preventDefault();
      const first = firstEnabledIndex();
      if (first >= 0) setActiveIndex(first);
    } else if (event.key === "End" && open && options.length > 0) {
      event.preventDefault();
      const last = nextEnabledIndex(0, -1);
      if (last >= 0) setActiveIndex(last);
    } else if (event.key === "Enter" && open && options[activeOptionIndex]) {
      event.preventDefault();
      choose(options[activeOptionIndex].code);
    } else if (event.key === "Escape" && open) {
      event.preventDefault();
      event.stopPropagation();
      setQuery("");
      setOpen(false);
    }
  };

  return (
    <div className={`target-language-picker${className ? ` ${className}` : ""}`}>
      <input
        id={id}
        className="target-language-picker__input"
        type="text"
        role="combobox"
        aria-label={ariaLabel}
        aria-autocomplete="list"
        aria-expanded={open && !disabled}
        aria-controls={open && !disabled && options.length > 0 ? `${id}-listbox` : undefined}
        aria-activedescendant={open && !disabled && activeOptionIndex >= 0 ? `${id}-option-${activeOptionIndex}` : undefined}
        autoComplete="off"
        // 無効中も選択済みの値は見せ続け、何が保存されているか分かるようにする。
        disabled={disabled}
        value={open && !disabled ? query : selectedLabel}
        placeholder={t("common.targetLanguageSearch")}
        data-pointer-focus={pointerFocused || undefined}
        onPointerDown={() => {
          pointerDownRef.current = true;
        }}
        onFocus={() => {
          setPointerFocused(pointerDownRef.current);
          pointerDownRef.current = false;
          if (disabled) return;
          setQuery("");
          setOpen(true);
          setActiveIndex(firstEnabledIndex());
        }}
        onClick={() => {
          // Escで閉じた後も入力欄はフォーカスを持ったままなので、focus だけでは開き直せない。
          // 押したら必ず候補を出し、閉じた状態で固まらないようにする。
          if (disabled || open) return;
          setQuery("");
          setOpen(true);
          setActiveIndex(firstEnabledIndex());
        }}
        onChange={(event) => {
          if (disabled) return;
          setQuery(event.currentTarget.value);
          setActiveIndex(firstEnabledIndex());
          setOpen(true);
        }}
        onKeyDown={handleKeyDown}
        onBlur={(event) => {
          // フォーカス中に押された分の印が残ると、次に Tab で入ったときにリングが消えるため、ここで捨てる。
          pointerDownRef.current = false;
          setPointerFocused(false);
          if (event.relatedTarget instanceof Node && event.currentTarget.parentElement?.contains(event.relatedTarget)) return;
          setOpen(false);
          setQuery("");
        }}
      />
      {open && !disabled && options.length > 0 && (
        <ul className="target-language-picker__list" id={`${id}-listbox`} role="listbox" aria-label={ariaLabel} ref={listRef}>
          {options.map((language, index) => (
            <li
              className="target-language-picker__option"
              id={`${id}-option-${index}`}
              key={language.code}
              role="option"
              aria-selected={language.code === value}
              aria-disabled={isDisabled(language.code) || undefined}
              data-active={index === activeOptionIndex || undefined}
              tabIndex={-1}
              onMouseEnter={() => { if (!isDisabled(language.code)) setActiveIndex(index); }}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(language.code)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  choose(language.code);
                }
              }}
            >
              <span className="target-language-picker__name" dir="auto">{language.name}</span>
              <span className="target-language-picker__region" dir="auto">{language.region}</span>
              <code>{language.code}</code>
              {isDisabled(language.code) && disabledOptionReason && (
                <span className="target-language-picker__unavailable">{disabledOptionReason}</span>
              )}
            </li>
          ))}
        </ul>
      )}
      {open && !disabled && options.length === 0 && (
        <output className="target-language-picker__list target-language-picker__empty" aria-live="polite">
          {t("common.noLanguagesFound")}
        </output>
      )}
    </div>
  );
}
