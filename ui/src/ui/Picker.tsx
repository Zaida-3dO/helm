// The picker: a trigger plus a listbox popover, optionally searchable. Used for model,
// effort, fonts, devices. Escape closes it through the layer stack (kind "popover"); a search field
// holding text is cleared first (kind "search").
//
// The popover itself is `PickerPopover`, the one look every picker shares: a heading with an `esc`
// key hint, a search field with a magnifier, rows with a highlight track and accent bar, a tick on
// the current value, and a key-hint footer. A control that needs its own trigger (or an extra row
// above the hints, like the effort stops) uses `PickerPopover` directly; `Picker` is the plain
// trigger-plus-popover pairing.

import { useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './controls.tsx';
import { useLayer } from './hooks.ts';
import { layoutSize, nextIndex, pageZoom, placeBelow, unzoom, viewportAt } from './place.ts';

export interface PickerOption<T extends string> {
  value: T;
  title: ReactNode;
  /** Plain text used for search; defaults to the title when it is a string. */
  text?: string;
  subtitle?: ReactNode;
  disabled?: boolean;
}

export interface PickerProps<T extends string> {
  value: T | null;
  options: PickerOption<T>[];
  onChange: (v: T) => void;
  label: string;
  /** The popover's heading; defaults to the label. */
  heading?: string;
  searchable?: boolean;
  searchPlaceholder?: string;
  /** Larger trigger for settings rows (34u instead of 29u). */
  settings?: boolean;
  /**
   * The composer toolbar's trigger: gap 7, max width 230 (180 at 1000 px and below,
   * 150 at 760 px and below), ellipsis. Pair it with `display` for a unified "Model · Effort" label.
   */
  compact?: boolean;
  /** What the trigger shows instead of the chosen option's title. */
  display?: ReactNode;
  /** The trigger's tooltip. */
  title?: string;
  disabled?: boolean;
  placeholder?: string;
  /** An extra class on the trigger, for a place that needs it smaller or outlined. */
  className?: string;
  testId?: string;
}

/** Options matching a search (case-insensitive substring on the option's text). */
export function filterOptions<T extends string>(options: PickerOption<T>[], q: string): PickerOption<T>[] {
  const s = q.trim().toLowerCase();
  if (!s) return options;
  return options.filter((o) => (o.text ?? (typeof o.title === 'string' ? o.title : o.value)).toLowerCase().includes(s));
}

export interface PickerPopoverProps<T extends string> {
  /** The control the popover opens from; it is placed below it (above when there is no room). */
  anchor: RefObject<HTMLElement | null>;
  value: T | null;
  options: PickerOption<T>[];
  /** Called with the chosen option's value; the popover does not close itself. */
  onPick: (v: T) => void;
  onClose: () => void;
  label: string;
  /** The heading row; it carries an `esc` key hint. */
  heading?: string;
  searchable?: boolean;
  searchPlaceholder?: string;
  /** An extra row between the list and the key hints. */
  footer?: ReactNode;
  /** An extra key hint between "navigate" and "select". */
  extraHint?: ReactNode;
  testId?: string;
  className?: string;
}

/** The shared picker popover (see the file comment). Mount it while open. */
export function PickerPopover<T extends string>({
  anchor,
  value,
  options,
  onPick,
  onClose,
  label,
  heading,
  searchable,
  searchPlaceholder = 'Search',
  footer,
  extraHint,
  testId,
  className,
}: PickerPopoverProps<T>) {
  const pop = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const [q, setQ] = useState('');
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const shown = useMemo(() => filterOptions(options, q), [options, q]);
  const [active, setActive] = useState(() => Math.max(0, options.findIndex((o) => o.value === value)));
  useLayer('popover', true, onClose);
  useLayer('search', true, () => setQ(''), () => q.length > 0);
  useLayoutEffect(() => {
    const a = anchor.current?.getBoundingClientRect();
    const z = pageZoom();
    if (a && pop.current) setPos(placeBelow(unzoom(a, z), layoutSize(pop.current), viewportAt(z), 4));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // The popover is hidden until placed, and a hidden field cannot take focus: focus it once shown.
  useLayoutEffect(() => {
    if (pos) (search.current ?? pop.current?.querySelector<HTMLElement>('[role="listbox"]'))?.focus();
  }, [pos]);
  useLayoutEffect(() => {
    pop.current?.querySelector('.ui-picker-option.active')?.scrollIntoView?.({ block: 'nearest' });
  }, [active]);
  const pick = (o: PickerOption<T> | undefined) => {
    if (o && !o.disabled) onPick(o.value);
  };
  return createPortal(
    <>
      <div className="ui-dismiss clear" onMouseDown={onClose} />
      <div
        ref={pop}
        className={`ui-picker-pop${searchable ? ' searchable' : ''}${className ? ` ${className}` : ''}`}
        data-testid={testId}
        style={{ left: pos?.left ?? 0, top: pos?.top ?? 0, visibility: pos ? 'visible' : 'hidden' }}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            setActive(nextIndex(active, e.key === 'ArrowDown' ? 1 : -1, shown.map((o) => !!o.disabled)));
          } else if (e.key === 'Enter') {
            e.preventDefault();
            pick(shown[active]);
          }
        }}
      >
        {heading && (
          <div className="ui-picker-heading">
            <span>{heading}</span>
            <kbd>esc</kbd>
          </div>
        )}
        {searchable && (
          <div className="ui-picker-searchbox">
            <span className="ui-picker-search-icon" aria-hidden="true">
              <Icon name="search" size={13} />
            </span>
            <input
              ref={search}
              className="ui-picker-search"
              placeholder={searchPlaceholder}
              aria-label={searchPlaceholder}
              value={q}
              onChange={(e) => {
                setQ(e.target.value);
                setActive(0);
              }}
            />
          </div>
        )}
        <div className="ui-picker-list" role="listbox" aria-label={label} tabIndex={searchable ? -1 : 0}>
          {shown.length === 0 && <div className="ui-picker-empty">No matches</div>}
          {shown.map((o, i) => (
            <div
              key={o.value}
              role="option"
              aria-selected={o.value === value}
              aria-disabled={o.disabled || undefined}
              className={`ui-picker-option${i === active ? ' active' : ''}`}
              data-value={o.value}
              onMouseEnter={() => setActive(i)}
              onClick={() => pick(o)}
            >
              <div className="ui-picker-text">
                <div className="ui-picker-title">{o.title}</div>
                {o.subtitle && <div className="ui-picker-sub">{o.subtitle}</div>}
              </div>
              {o.value === value && <Icon name="check" />}
            </div>
          ))}
        </div>
        {footer}
        <div className="ui-picker-hints">
          <span>↑ ↓ navigate</span>
          {extraHint}
          <span>↵ select</span>
        </div>
      </div>
    </>,
    document.body,
  );
}

export function Picker<T extends string>({
  value,
  options,
  onChange,
  label,
  heading,
  searchable,
  searchPlaceholder,
  settings,
  compact,
  display,
  title,
  disabled,
  placeholder,
  className,
  testId,
}: PickerProps<T>) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const close = () => {
    setOpen(false);
    trigger.current?.focus();
  };
  const current = options.find((o) => o.value === value);
  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={`ui-picker-trigger${settings ? ' settings' : ''}${compact ? ' compact' : ''}${className ? ` ${className}` : ''}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={label}
        title={title}
        disabled={disabled}
        data-testid={testId}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open && !disabled) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span className="ui-picker-value">{display ?? current?.title ?? placeholder ?? 'Choose…'}</span>
        <span className={`ui-picker-chevron${open ? ' open' : ''}`}>
          <Icon name="chevron-down" size={12} />
        </span>
      </button>
      {open && (
        <PickerPopover
          anchor={trigger}
          value={value}
          options={options}
          label={label}
          heading={heading ?? label}
          searchable={searchable}
          searchPlaceholder={searchPlaceholder}
          onClose={close}
          onPick={(v) => {
            onChange(v);
            close();
          }}
        />
      )}
    </>
  );
}
