import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { compareText, lowerText, useT, useUiLanguage } from "../i18n";
import { findMinecraftLanguage, MINECRAFT_LANGUAGES, minecraftLanguageLabel, targetLanguageName } from "../i18n/minecraftLanguages";
import "./TargetLanguagePicker.css";

/* oxlint-disable jsx-a11y/prefer-tag-over-role, jsx-a11y/no-noninteractive-element-to-interactive-role -- 検索候補はactive descendantによるキーボード操作のためARIA roleで表す。 */

interface TargetLanguagePickerProps {
  "aria-label": string;
  className?: string;
  id?: string;
  value: string;
  onChange: (value: string) => void;
}

export function TargetLanguagePicker({ "aria-label": ariaLabel, className, id: givenId, value, onChange }: TargetLanguagePickerProps) {
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

  useEffect(() => {
    if (open && activeOptionIndex >= 0) {
      listRef.current?.querySelectorAll<HTMLElement>("[role=option]")[activeOptionIndex]?.scrollIntoView?.({ block: "nearest" });
    }
  }, [activeOptionIndex, open]);

  const choose = (code: string) => {
    onChange(code);
    setQuery("");
    setOpen(false);
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!open) {
        setOpen(true);
        setActiveIndex(0);
      } else if (options.length > 0) {
        setActiveIndex((current) => (current + (event.key === "ArrowDown" ? 1 : -1) + options.length) % options.length);
      }
    } else if (event.key === "Home" && open && options.length > 0) {
      event.preventDefault();
      setActiveIndex(0);
    } else if (event.key === "End" && open && options.length > 0) {
      event.preventDefault();
      setActiveIndex(options.length - 1);
    } else if (event.key === "Enter" && open && options[activeIndex]) {
      event.preventDefault();
      choose(options[activeIndex].code);
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
        aria-expanded={open}
        aria-controls={open && options.length > 0 ? `${id}-listbox` : undefined}
        aria-activedescendant={open && activeOptionIndex >= 0 ? `${id}-option-${activeOptionIndex}` : undefined}
        autoComplete="off"
        value={open ? query : selectedLabel}
        placeholder={t("common.targetLanguageSearch")}
        data-pointer-focus={pointerFocused || undefined}
        onPointerDown={() => {
          pointerDownRef.current = true;
        }}
        onFocus={() => {
          setPointerFocused(pointerDownRef.current);
          pointerDownRef.current = false;
          setQuery("");
          setOpen(true);
          setActiveIndex(0);
        }}
        onChange={(event) => {
          setQuery(event.currentTarget.value);
          setActiveIndex(0);
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
      {open && options.length > 0 && (
        <ul className="target-language-picker__list" id={`${id}-listbox`} role="listbox" aria-label={ariaLabel} ref={listRef}>
          {options.map((language, index) => (
            <li
              className="target-language-picker__option"
              id={`${id}-option-${index}`}
              key={language.code}
              role="option"
              aria-selected={language.code === value}
              data-active={index === activeOptionIndex || undefined}
              tabIndex={-1}
              onMouseEnter={() => setActiveIndex(index)}
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
            </li>
          ))}
        </ul>
      )}
      {open && options.length === 0 && (
        <output className="target-language-picker__list target-language-picker__empty" aria-live="polite">
          {t("common.noLanguagesFound")}
        </output>
      )}
    </div>
  );
}
