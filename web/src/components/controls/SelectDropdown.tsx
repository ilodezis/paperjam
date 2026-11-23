import { Check, ChevronDown } from "lucide-react";
import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";

const MENU_MAX_HEIGHT = 260;
const MENU_GAP = 4;
const VIEWPORT_MARGIN = 8;
const MIN_MENU_HEIGHT = 80;

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface SelectDropdownProps {
  value: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  /** Shown on the trigger while no option is chosen. It is not an option in the list: a value
   *  that is required is never offered a "choose…" row, and the list only ever holds real options. */
  placeholder?: string;
  disabled?: boolean;
  name?: string;
  "aria-label"?: string;
  className?: string;
  id?: string;
}

interface MenuPlacement {
  up: boolean;
  maxHeight: number;
}

/** A select-only combobox (ARIA 1.2): one button that owns the focus, names the control and
 *  announces the chosen value, and a listbox it points at with `aria-activedescendant`. There is
 *  no second, native select behind it; a `name` is serialized by a hidden input. */
export function SelectDropdown({
  value,
  onChange,
  options,
  placeholder,
  disabled = false,
  name,
  "aria-label": ariaLabel,
  className = "",
  id,
}: SelectDropdownProps) {
  const [open, setOpen] = useState(false);
  const [focusedIndex, setFocusedIndex] = useState(-1);
  const [placement, setPlacement] = useState<MenuPlacement>({ up: false, maxHeight: MENU_MAX_HEIGHT });
  const containerRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const listId = useId();

  const selectedOption = options.find((opt) => opt.value === value);
  const optionId = (index: number) => `${listId}-${index}`;

  useLayoutEffect(() => {
    if (!open) return;
    const measure = () => {
      const container = containerRef.current;
      const trigger = buttonRef.current;
      if (!container || !trigger) return;
      const rect = trigger.getBoundingClientRect();
      const insideSheet = container.closest(".sheet") !== null;
      const bnav = !insideSheet ? document.querySelector<HTMLElement>(".bnav") : null;
      const bnavRect = bnav?.getBoundingClientRect();
      const bottomLimit = bnavRect && bnavRect.height > 0 ? Math.min(window.innerHeight, bnavRect.top) : window.innerHeight;
      const spaceBelow = Math.max(0, bottomLimit - rect.bottom - MENU_GAP - VIEWPORT_MARGIN);
      const spaceAbove = Math.max(0, rect.top - MENU_GAP - VIEWPORT_MARGIN);
      const desired = Math.min(MENU_MAX_HEIGHT, listRef.current?.scrollHeight || options.length * 40 + 8);
      const up = spaceBelow < desired && spaceAbove > spaceBelow;
      const available = up ? spaceAbove : spaceBelow;
      const maxHeight = Math.min(MENU_MAX_HEIGHT, Math.max(Math.min(MIN_MENU_HEIGHT, available || MIN_MENU_HEIGHT), Math.floor(available)));
      setPlacement((prev) => (prev.up === up && prev.maxHeight === maxHeight ? prev : { up, maxHeight }));
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [open, options.length]);

  useEffect(() => {
    if (!open) return;
    function onPointerDown(event: Event) {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  useEffect(() => {
    if (open && focusedIndex >= 0) {
      document.getElementById(optionId(focusedIndex))?.scrollIntoView?.({ block: "nearest" });
    }
    // optionId only depends on listId
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, focusedIndex]);

  const enabled = (index: number) => index >= 0 && index < options.length && !options[index].disabled;

  /** The next enabled option from `from` in `step` direction, wrapping around. */
  function move(from: number, step: 1 | -1): number {
    for (let i = 1; i <= options.length; i += 1) {
      const next = (((from + step * i) % options.length) + options.length) % options.length;
      if (enabled(next)) return next;
    }
    return -1;
  }

  function openList() {
    const selected = options.findIndex((opt) => opt.value === value);
    setFocusedIndex(enabled(selected) ? selected : move(-1, 1));
    setOpen(true);
  }

  function closeList() {
    setOpen(false);
    buttonRef.current?.focus();
  }

  function handleSelect(optionValue: string) {
    onChange(optionValue);
    closeList();
  }

  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (disabled) return;

    if (!open) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        openList();
      }
      return;
    }

    switch (event.key) {
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        closeList();
        break;
      case "ArrowDown":
        event.preventDefault();
        setFocusedIndex((prev) => move(prev, 1));
        break;
      case "ArrowUp":
        event.preventDefault();
        setFocusedIndex((prev) => move(prev, -1));
        break;
      case "Home":
        event.preventDefault();
        setFocusedIndex(move(-1, 1));
        break;
      case "End":
        event.preventDefault();
        setFocusedIndex(move(0, -1));
        break;
      case "Enter":
      case " ":
        event.preventDefault();
        if (enabled(focusedIndex)) handleSelect(options[focusedIndex].value);
        break;
      case "Tab":
        setOpen(false);
        break;
    }
  }

  return (
    <div
      ref={containerRef}
      className={`select-dropdown-container ${className} ${open ? "select-dropdown--open" : ""}`}
      id={id}
    >
      {name ? <input type="hidden" name={name} value={value} disabled={disabled} /> : null}

      <button
        ref={buttonRef}
        type="button"
        role="combobox"
        className={`select-dropdown-trigger ${!selectedOption ? "select-dropdown-trigger--placeholder" : ""}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-activedescendant={open && focusedIndex >= 0 ? optionId(focusedIndex) : undefined}
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => {
          if (disabled) return;
          if (open) setOpen(false);
          else openList();
        }}
        onKeyDown={handleKeyDown}
      >
        <span className="select-dropdown-value">{selectedOption ? selectedOption.label : (placeholder ?? "")}</span>
        <ChevronDown className={`select-chevron ${open ? "select-chevron--open" : ""}`} aria-hidden="true" />
      </button>

      {open ? (
        <ul
          ref={listRef}
          id={listId}
          className={`select-dropdown-menu ${placement.up ? "select-dropdown-menu--up" : ""}`}
          style={{ maxHeight: `${placement.maxHeight}px` }}
          role="listbox"
          aria-label={ariaLabel}
        >
          {options.map((opt, index) => {
            const isSelected = opt.value === value;
            const isFocused = index === focusedIndex;
            return (
              <li
                key={opt.value}
                id={optionId(index)}
                role="option"
                aria-selected={isSelected}
                aria-disabled={opt.disabled || undefined}
                className={`select-dropdown-item ${isSelected ? "select-dropdown-item--selected" : ""} ${
                  isFocused ? "select-dropdown-item--focused" : ""
                } ${opt.disabled ? "select-dropdown-item--disabled" : ""}`}
                onClick={(event) => {
                  event.stopPropagation();
                  if (!opt.disabled) handleSelect(opt.value);
                }}
              >
                <span className="select-dropdown-label">{opt.label}</span>
                {isSelected ? <Check className="select-check" aria-hidden="true" /> : null}
              </li>
            );
          })}
        </ul>
      ) : null}
    </div>
  );
}
