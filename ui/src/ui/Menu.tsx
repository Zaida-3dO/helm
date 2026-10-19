// The context menu: a transparent dismiss layer, then the menu at the
// pointer (or under a trigger). Arrow keys move, Enter or Space chooses, Escape closes through the
// layer stack (kind "menu"). One submenu level opens on hover or ArrowRight.

import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './controls.tsx';
import { useLayer } from './hooks.ts';
import { keys } from './keys.ts';
import { layoutSize, nextIndex, pageZoom, placeAtPoint, viewportAt } from './place.ts';

export interface MenuItem {
  id: string;
  label: ReactNode;
  icon?: ReactNode;
  /** A chord hint, shown right-aligned. Prefer `command`, which can never go stale. */
  shortcut?: string;
  /** A key-registry command id: the hint shows its live chord (and nothing when it is unbound). */
  command?: string;
  disabled?: boolean;
  danger?: boolean;
  /** A separator above this item. */
  separator?: boolean;
  /** A ticked item (radio or checkbox entries). */
  checked?: boolean;
  submenu?: MenuItem[];
  onSelect?: () => void;
}

export interface ContextMenuProps {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
  label?: string;
  testId?: string;
}

export function ContextMenu({ x, y, items, onClose, label, testId }: ContextMenuProps) {
  const el = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  const [active, setActive] = useState(-1);
  const [sub, setSub] = useState<number | null>(null);
  useLayer('menu', true, () => {
    if (sub !== null) setSub(null);
    else onClose();
  });
  useLayoutEffect(() => {
    const z = pageZoom();
    if (el.current) setPos(placeAtPoint(x / z, y / z, layoutSize(el.current), viewportAt(z)));
    el.current?.focus();
  }, [x, y]);
  const choose = (it: MenuItem, i: number) => {
    if (it.disabled) return;
    if (it.submenu) {
      setSub(i);
      return;
    }
    onClose();
    it.onSelect?.();
  };
  const disabled = items.map((i) => !!i.disabled);
  const hint = (it: MenuItem) => (it.command !== undefined ? keys.chordFor(it.command) : it.shortcut);
  return createPortal(
    <>
      <div className="ui-dismiss" onMouseDown={onClose} onContextMenu={(e) => (e.preventDefault(), onClose())} />
      <div
        ref={el}
        className="ui-menu"
        role="menu"
        aria-label={label}
        tabIndex={-1}
        data-testid={testId}
        style={{ left: pos.left, top: pos.top }}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            setActive(nextIndex(active, e.key === 'ArrowDown' ? 1 : -1, disabled));
          } else if ((e.key === 'Enter' || e.key === ' ') && active >= 0) {
            e.preventDefault();
            const it = items[active];
            if (it) choose(it, active);
          } else if (e.key === 'ArrowRight' && active >= 0 && items[active]?.submenu) {
            e.preventDefault();
            setSub(active);
          } else if (e.key === 'ArrowLeft' && sub !== null) {
            e.preventDefault();
            setSub(null);
          }
        }}
      >
        {items.map((it, i) => (
          <div key={it.id} className="ui-menu-slot">
            {it.separator && <div className="ui-menu-sep" role="separator" />}
            <button
              type="button"
              role={it.checked !== undefined ? 'menuitemcheckbox' : 'menuitem'}
              aria-checked={it.checked}
              aria-haspopup={it.submenu ? 'menu' : undefined}
              aria-expanded={it.submenu ? sub === i : undefined}
              className={`ui-menu-item${i === active ? ' active' : ''}${it.danger ? ' ui-danger' : ''}${sub === i ? ' open' : ''}`}
              disabled={it.disabled}
              data-item={it.id}
              onMouseEnter={() => {
                setActive(i);
                if (it.submenu) setSub(i);
                else if (sub !== null) setSub(null);
              }}
              onClick={() => choose(it, i)}
            >
              <span className="ui-menu-icon">{it.checked ? <Icon name="check" /> : it.icon}</span>
              <span className="ui-menu-label">{it.label}</span>
              {hint(it) && <kbd>{hint(it)}</kbd>}
              {it.submenu && <Icon name="chevron-right" size={12} />}
            </button>
            {it.submenu && sub === i && (
              <div className="ui-menu ui-submenu" role="menu">
                {it.submenu.map((s) => (
                  <button
                    key={s.id}
                    type="button"
                    role={s.checked !== undefined ? 'menuitemradio' : 'menuitem'}
                    aria-checked={s.checked}
                    className={`ui-menu-item${s.danger ? ' ui-danger' : ''}`}
                    disabled={s.disabled}
                    data-item={s.id}
                    onClick={() => {
                      onClose();
                      s.onSelect?.();
                    }}
                  >
                    <span className="ui-menu-icon">{s.checked ? <Icon name="check" /> : s.icon}</span>
                    <span className="ui-menu-label">{s.label}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        ))}
      </div>
    </>,
    document.body,
  );
}
