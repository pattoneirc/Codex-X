import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { Check, ChevronDown } from "lucide-react";

import { cx } from "./utils";

export type SelectOption = {
  value: string;
  label: string;
  description?: string;
  badge?: string;
  disabled?: boolean;
};
export type FormControlOption = SelectOption;

export interface SelectProps {
  value: string;
  onValueChange: (value: string) => void;
  options: readonly SelectOption[];
  placeholder?: string;
  disabled?: boolean;
  id?: string;
  className?: string;
  size?: "md" | "sm";
  "aria-label"?: string;
  "aria-labelledby"?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
  emptyText?: string;
}
export type ComboBoxProps = SelectProps;

type PopupPosition = { top: number; left: number; width: number; maxHeight: number };

function matchesQuery(option: SelectOption, query: string) {
  const text = query.trim().toLocaleLowerCase();
  return !text || `${option.label}\n${option.value}\n${option.description ?? ""}`.toLocaleLowerCase().includes(text);
}

function visibleAnchor(anchor: HTMLElement, rect: DOMRect, viewport: { top: number; left: number; bottom: number; right: number }) {
  if (!anchor.isConnected || !anchor.getClientRects().length || rect.width <= 0 || rect.height <= 0
    || rect.bottom <= viewport.top || rect.top >= viewport.bottom
    || rect.right <= viewport.left || rect.left >= viewport.right) return false;
  for (let element: HTMLElement | null = anchor; element; element = element.parentElement) {
    const style = window.getComputedStyle(element);
    if (element.hidden || element.getAttribute("aria-hidden") === "true" || element.hasAttribute("inert")
      || style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse") return false;
    if (element === anchor || element === document.body || element === document.documentElement) continue;
    const bounds = element.getBoundingClientRect();
    if (/(auto|scroll|hidden|clip)/.test(style.overflowY) && (rect.bottom <= bounds.top || rect.top >= bounds.bottom)) return false;
    if (/(auto|scroll|hidden|clip)/.test(style.overflowX) && (rect.right <= bounds.left || rect.left >= bounds.right)) return false;
  }
  return true;
}

function ChoiceControl({
  value, onValueChange, options, placeholder = "", disabled = false, id, className, size = "md",
  "aria-label": ariaLabel, "aria-labelledby": labelledBy, "aria-describedby": describedBy,
  "aria-invalid": invalid, emptyText = "No matching options", editable,
}: SelectProps & { editable: boolean }) {
  const generatedId = useId();
  const listId = `${generatedId}-listbox`;
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const popupRef = useRef<HTMLDivElement>(null);
  const composing = useRef(false);
  const pointerInPopup = useRef(false);
  const navigationIntent = useRef(false);
  const typeahead = useRef({ text: "", at: 0 });
  const [open, setOpen] = useState(false);
  const [filtering, setFiltering] = useState(false);
  const [query, setQuery] = useState("");
  const [activeValue, setActiveValue] = useState<string | null>(null);
  const [position, setPosition] = useState<PopupPosition | null>(null);
  const [theme, setTheme] = useState("");
  const selected = options.find(option => option.value === value);
  const visible = useMemo(() => editable && filtering
    ? options.filter(option => matchesQuery(option, query)) : [...options], [editable, filtering, options, query]);
  const activeIndex = visible.findIndex(option => option.value === activeValue && !option.disabled);
  const activeId = open && activeIndex >= 0 ? `${listId}-${activeIndex}` : undefined;

  const focusAnchor = () => {
    const anchor = editable ? inputRef.current : buttonRef.current;
    if (anchor?.isConnected) anchor.focus({ preventScroll: true });
  };
  const close = () => { navigationIntent.current = false; setOpen(false); setPosition(null); setFiltering(false); };
  const show = (edge?: "first" | "last") => {
    if (disabled) return;
    setFiltering(false);
    setQuery("");
    navigationIntent.current = editable && Boolean(edge);
    const enabled = options.filter(option => !option.disabled);
    const current = selected && !selected.disabled ? selected : undefined;
    setActiveValue((current ?? (editable && !edge ? undefined : edge === "last" ? enabled[enabled.length - 1] : enabled[0]))?.value ?? null);
    setOpen(true);
  };
  const choose = (option: SelectOption) => {
    if (disabled || option.disabled) return;
    close();
    focusAnchor();
    onValueChange(option.value);
  };

  useEffect(() => { if (disabled) close(); }, [disabled]);
  useEffect(() => {
    if (open && activeIndex < 0 && (!editable || navigationIntent.current)) {
      setActiveValue(visible.find(option => !option.disabled)?.value ?? null);
    }
  }, [activeIndex, editable, open, visible]);
  useLayoutEffect(() => {
    if (open && activeId && position) document.getElementById(activeId)?.scrollIntoView({ block: "nearest" });
  }, [activeId, open, position]);

  useLayoutEffect(() => {
    if (!open) return;
    let frame = 0;
    let disposed = false;
    let previousPosition: PopupPosition | null = null;
    let previousTheme: string | null = null;
    const visualViewport = window.visualViewport;
    const update = () => {
      const anchor = rootRef.current;
      const popup = popupRef.current;
      if (disposed || !anchor || !popup) return;
      const visual = window.visualViewport;
      const viewport = {
        top: visual?.offsetTop ?? 0,
        left: visual?.offsetLeft ?? 0,
        bottom: (visual?.offsetTop ?? 0) + (visual?.height ?? window.innerHeight),
        right: (visual?.offsetLeft ?? 0) + (visual?.width ?? window.innerWidth),
      };
      const rect = anchor.getBoundingClientRect();
      if (!visibleAnchor(anchor, rect, viewport)) { close(); return; }
      const margin = 8;
      const gap = 6;
      const width = Math.max(0, Math.min(Math.max(rect.width, 220), viewport.right - viewport.left - margin * 2));
      const left = Math.max(viewport.left + margin, Math.min(rect.left, viewport.right - width - margin));
      const above = rect.top - viewport.top - margin - gap;
      const below = viewport.bottom - rect.bottom - margin - gap;
      const desiredHeight = Math.min(popup.scrollHeight + 2, 320);
      const upward = below < desiredHeight && above > below;
      const maxHeight = Math.max(0, Math.min(320, upward ? above : below));
      if (maxHeight < 24 || width < 24) { close(); return; }
      const top = upward ? rect.top - gap - Math.min(desiredHeight, maxHeight) : rect.bottom + gap;
      if (!previousPosition || previousPosition.top !== top || previousPosition.left !== left
        || previousPosition.width !== width || previousPosition.maxHeight !== maxHeight) {
        previousPosition = { top, left, width, maxHeight };
        setPosition(previousPosition);
      }
      const scopedTheme = anchor.closest("[data-theme]")?.getAttribute("data-theme")
        || (anchor.closest(".theme-light") ? "light" : anchor.closest(".theme-dark") ? "dark" : "");
      if (previousTheme !== scopedTheme) { previousTheme = scopedTheme; setTheme(scopedTheme); }
    };
    // Layout shifts and page transitions can move an anchor without a scroll
    // or resize. Only an open menu watches its geometry; teardown cancels it.
    const watch = () => { update(); if (!disposed) frame = window.requestAnimationFrame(watch); };
    watch();
    const onOutside = (event: PointerEvent) => {
      const node = event.target instanceof Node ? event.target : null;
      if (node && !rootRef.current?.contains(node) && !popupRef.current?.contains(node)) close();
    };
    const onFocus = (event: FocusEvent) => {
      const node = event.target instanceof Node ? event.target : null;
      if (node && !rootRef.current?.contains(node) && !popupRef.current?.contains(node) && !pointerInPopup.current) close();
    };
    const onVisibility = () => { if (document.hidden) close(); };
    const onPointerEnd = () => {
      const wasInside = pointerInPopup.current;
      pointerInPopup.current = false;
      if (wasInside) focusAnchor();
    };
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    window.addEventListener("blur", close);
    visualViewport?.addEventListener("resize", update);
    visualViewport?.addEventListener("scroll", update);
    document.addEventListener("pointerdown", onOutside, true);
    document.addEventListener("pointerup", onPointerEnd);
    document.addEventListener("pointercancel", onPointerEnd);
    document.addEventListener("focusin", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      window.cancelAnimationFrame(frame);
      pointerInPopup.current = false;
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
      window.removeEventListener("blur", close);
      visualViewport?.removeEventListener("resize", update);
      visualViewport?.removeEventListener("scroll", update);
      document.removeEventListener("pointerdown", onOutside, true);
      document.removeEventListener("pointerup", onPointerEnd);
      document.removeEventListener("pointercancel", onPointerEnd);
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [open]);

  const handleKey = (event: KeyboardEvent<HTMLInputElement | HTMLButtonElement>) => {
    if (composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) {
      // Let the IME cancel composition without letting its Escape dismiss a dialog.
      if (event.key === "Escape") event.stopPropagation();
      return;
    }
    if (disabled) return;
    if (event.key === "Escape" && open) {
      event.preventDefault(); event.stopPropagation(); close(); return;
    }
    if (event.key === "Tab") { if (open) close(); return; }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!open) { show(event.key === "ArrowUp" ? "last" : "first"); return; }
      navigationIntent.current = true;
      const direction = event.key === "ArrowDown" ? 1 : -1;
      for (let step = 1; step <= visible.length; step += 1) {
        const index = ((activeIndex < 0 ? (direction === 1 ? -1 : 0) : activeIndex) + direction * step + visible.length) % visible.length;
        if (!visible[index].disabled) { setActiveValue(visible[index].value); break; }
      }
      return;
    }
    if (!editable && (event.key === "Home" || event.key === "End")) {
      event.preventDefault();
      if (!open) show();
      const enabled = options.filter(option => !option.disabled);
      setActiveValue((event.key === "Home" ? enabled[0] : enabled[enabled.length - 1])?.value ?? null);
      return;
    }
    if (event.key === "Enter" || (!editable && event.key === " ")) {
      event.preventDefault();
      if (editable) {
        const typed = inputRef.current?.value ?? value;
        const exact = visible.find(option => !option.disabled && option.value === typed);
        if (open && navigationIntent.current && activeIndex >= 0) choose(visible[activeIndex]);
        else if (open && exact) choose(exact);
        else close();
        return;
      }
      if (!open) show();
      else if (activeIndex >= 0) choose(visible[activeIndex]);
      else close();
      return;
    }
    if (!editable && event.key.length === 1 && !event.altKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      const now = Date.now();
      typeahead.current = { text: `${now - typeahead.current.at < 700 ? typeahead.current.text : ""}${event.key}`.toLocaleLowerCase(), at: now };
      const match = options.find(option => !option.disabled && option.label.toLocaleLowerCase().startsWith(typeahead.current.text));
      if (!open) show();
      if (match) setActiveValue(match.value);
    }
  };
  const onBlur = () => {
    if (!pointerInPopup.current) close();
  };
  const accessibility = {
    id,
    role: "combobox" as const,
    "aria-label": ariaLabel,
    "aria-labelledby": labelledBy,
    "aria-describedby": describedBy,
    "aria-invalid": invalid,
    "aria-haspopup": "listbox" as const,
    "aria-expanded": open,
    "aria-controls": open ? listId : undefined,
    "aria-activedescendant": activeId,
  };

  return <div ref={rootRef} className={cx("ui-select", editable && "ui-select--editable", `ui-select--${size}`, disabled && "ui-select--disabled", open && "ui-select--open", className)}>
    {editable ? <div className="ui-select__anchor" onMouseDown={event => {
      if (event.target === inputRef.current || disabled) return;
      event.preventDefault(); focusAnchor(); if (open) close(); else show();
    }}>
      <input {...accessibility} ref={inputRef} type="text" className="ui-select__input" value={value} placeholder={placeholder}
        disabled={disabled} autoComplete="off" spellCheck={false} aria-autocomplete="list" onKeyDown={handleKey} onBlur={onBlur}
        onFocus={() => { if (!open) show(); }} onClick={() => { if (!open) show(); }}
        onCompositionStart={() => { composing.current = true; navigationIntent.current = false; }}
        onCompositionEnd={event => { composing.current = false; setQuery(event.currentTarget.value); setFiltering(true); }}
        onChange={event => {
          const next = event.currentTarget.value;
          navigationIntent.current = false;
          setQuery(next); setFiltering(true); setOpen(true);
          setActiveValue(options.find(option => !option.disabled && option.value === next)?.value ?? null);
          onValueChange(next);
        }} title={value || placeholder} />
      <span className="ui-select__chevron" aria-hidden="true"><ChevronDown size={16} strokeWidth={1.8} /></span>
    </div> : <button {...accessibility} ref={buttonRef} type="button" className="ui-select__trigger" disabled={disabled}
      onKeyDown={handleKey} onBlur={onBlur} onClick={() => { if (open) close(); else show(); }} title={selected?.label || value || placeholder}>
      <span className={cx("ui-select__value", !selected && "ui-select__placeholder")}>{selected ? selected.label : value || placeholder}</span>
      <ChevronDown className="ui-select__chevron" size={16} strokeWidth={1.8} aria-hidden="true" />
    </button>}
    {open && typeof document !== "undefined" && createPortal(<div ref={popupRef} id={listId} role="listbox" aria-label={ariaLabel || placeholder || undefined}
      aria-labelledby={labelledBy} className={cx("ui-select-popup", `ui-select-popup--${size}`)} data-theme={theme || undefined}
      style={position ? { ...position, visibility: "visible" } : { top: 0, left: 0, visibility: "hidden" }}
      onPointerDownCapture={() => { pointerInPopup.current = true; }} onMouseDown={event => {
        if (event.target instanceof Element && event.target.closest('[role="option"]')) event.preventDefault();
      }}>
      {visible.length ? visible.map((option, index) => <div id={`${listId}-${index}`} key={`${option.value}:${index}`} role="option"
        aria-selected={option.value === value} aria-disabled={option.disabled || undefined}
        className={cx("ui-select-option", option.value === value && "ui-select-option--selected", index === activeIndex && "ui-select-option--active", option.disabled && "ui-select-option--disabled")}
        onPointerMove={event => { if (event.pointerType !== "touch" && !option.disabled) setActiveValue(option.value); }}
        onClick={event => { event.stopPropagation(); choose(option); }} title={[option.label, option.description, option.badge].filter(Boolean).join("\n")}>
        <span className="ui-select-option__copy"><span className="ui-select-option__heading"><span className="ui-select-option__label">{option.label}</span>
          {option.badge && <span className="ui-select-option__badge">{option.badge}</span>}</span>
          {option.description && <span className="ui-select-option__description">{option.description}</span>}</span>
        <span className="ui-select-option__check" aria-hidden="true">{option.value === value && <Check size={15} strokeWidth={2} />}</span>
      </div>) : <div className="ui-select-popup__empty">{emptyText}</div>}
    </div>, document.body)}
  </div>;
}

export function Select(props: SelectProps) { return <ChoiceControl {...props} editable={false} />; }
export function ComboBox(props: ComboBoxProps) { return <ChoiceControl {...props} editable />; }
