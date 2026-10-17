// The picker: a trigger plus a listbox popover, optionally searchable. Used for model,
// effort, fonts, devices. Escape closes it through the layer stack (kind "popover"); a search field
// holding text is cleared first (kind "search").

import { useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './controls.tsx';
import { useLayer } from './hooks.ts';
import { nextIndex, placeBelow } from './place.ts';

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
  heading?: string;
  searchable?: boolean;
  /** Larger trigger for settings rows (34u instead of 29u). */
  settings?: boolean;
  placeholder?: string;
  testId?: string;
}

/** Options matching a search (case-insensitive substring on the option's text). */
export function filterOptions<T extends string>(options: PickerOption<T>[], q: string): PickerOption<T>[] {
  const s = q.trim().toLowerCase();
  if (!s) return options;
  return options.filter((o) => (o.text ?? (typeof o.title === 'string' ? o.title : o.value)).toLowerCase().includes(s));
}

export function Picker<T extends string>({ value, options, onChange, label, heading, searchable, settings, placeholder, testId }: PickerProps<T>) {
  const trigger = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState('');
  const [active, setActive] = useState(-1);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const shown = useMemo(() => filterOptions(options, q), [options, q]);
  const close = () => {
    setOpen(false);
    setQ('');
    trigger.current?.focus();
  };
  useLayer('popover', open, close);
  useLayer('search', open, () => setQ(''), () => q.length > 0);
  useLayoutEffect(() => {
    if (!open) return;
    const a = trigger.current?.getBoundingClientRect();
    const r = pop.current?.getBoundingClientRect();
    if (a && r) setPos(placeBelow(a, { width: r.width, height: r.height }, { width: innerWidth, height: innerHeight }, 4));
    setActive(Math.max(0, shown.findIndex((o) => o.value === value)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
  const current = options.find((o) => o.value === value);
  const pick = (o: PickerOption<T> | undefined) => {
    if (!o || o.disabled) return;
    onChange(o.value);
    close();
  };
  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={`ui-picker-trigger${settings ? ' settings' : ''}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={label}
        data-testid={testId}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span className="ui-picker-value">{current?.title ?? placeholder ?? 'Choose…'}</span>
        <span className={`ui-picker-chevron${open ? ' open' : ''}`}>
          <Icon name="chevron-down" size={12} />
        </span>
      </button>
      {open &&
        createPortal(
          <>
            <div className="ui-dismiss clear" onMouseDown={close} />
            <div
              ref={pop}
              className={`ui-picker-pop${searchable ? ' searchable' : ''}`}
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
              {heading && <div className="ui-picker-heading">{heading}</div>}
              {searchable && (
                <input
                  className="ui-picker-search"
                  autoFocus
                  placeholder="Search"
                  value={q}
                  onChange={(e) => {
                    setQ(e.target.value);
                    setActive(0);
                  }}
                />
              )}
              <div className="ui-picker-list" role="listbox" aria-label={label} tabIndex={searchable ? -1 : 0} ref={(n) => (!searchable ? n?.focus() : undefined)}>
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
            </div>
          </>,
          document.body,
        )}
    </>
  );
}
