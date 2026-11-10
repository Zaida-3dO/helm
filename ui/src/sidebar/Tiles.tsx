// The tile row at the top of the sidebar: New and Search, then the views in most-recently-used
// order, and a More tile holding the views that do not fit. One row, never wrapping. The tiles are
// one roving group (Tab enters the row on New, the arrow keys, Home and End move along it) and
// Alt+Left / Alt+Right move a focused view tile.

import { useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from 'react';
import { ContextMenu, type MenuItem } from '../ui/index.ts';
import { IconMore } from '../ui/icons.tsx';
import { moveView, slotsFit, splitViews } from './tiles.ts';

export interface TileSpec {
  id: string;
  /** The one word under the icon, and the name in the More menu. */
  label: string;
  icon: ReactNode;
  testId: string;
  /** The tooltip: the full name, with its shortcut where it has one. */
  title: string;
  onActivate: () => void;
  badge?: { text: string; label: string; testId?: string } | null;
  /** A view that is open keeps its tile pressed. */
  pressed?: boolean;
  /** Not usable yet (the Flagged count is still loading). The tile stays, marked aria-disabled. */
  disabled?: boolean;
}

export interface TileRowProps {
  /** New and Search: always first. */
  lead: TileSpec[];
  /** The views in the order to show them. */
  views: TileSpec[];
  /** The views in their new order after Alt+arrow. */
  onOrder: (ids: string[]) => void;
  /** A view was opened. */
  onUsed: (id: string) => void;
}

/** The row's width before it has been measured: the sidebar's default width less its margins. */
const FIRST_WIDTH = 460;

export function TileRow({ lead, views, onOrder, onUsed }: TileRowProps) {
  const row = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(FIRST_WIDTH);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [focus, setFocus] = useState(0);

  useLayoutEffect(() => {
    const el = row.current;
    if (!el) return;
    const measure = () => setWidth(el.clientWidth);
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, []);

  const ids = views.map((v) => v.id);
  const { shown, more } = splitViews(ids, slotsFit(width));
  const byId = new Map(views.map((v) => [v.id, v]));
  const hidden = more.map((id) => byId.get(id)!);
  const total = lead.length + shown.length + (hidden.length > 0 ? 1 : 0);
  // The tile that takes Tab: the last focused, kept in range as tiles come and go.
  const rove = Math.min(focus, total - 1);

  const activate = (t: TileSpec, isView: boolean) => {
    if (t.disabled) return;
    t.onActivate();
    // Moving the tile after the view has opened keeps the click target still under the pointer.
    if (isView) setTimeout(() => onUsed(t.id), 0);
  };

  const onKeyDown = (e: ReactKeyboardEvent, spec: TileSpec | null, isView: boolean) => {
    if (e.altKey && isView && spec && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      e.preventDefault();
      onOrder(moveView(ids, spec.id, e.key === 'ArrowLeft' ? -1 : 1));
      return;
    }
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const all = [...(row.current?.querySelectorAll<HTMLElement>('[data-tile]') ?? [])];
    const at = all.indexOf(e.currentTarget as HTMLElement);
    const to = e.key === 'ArrowRight' ? at + 1 : e.key === 'ArrowLeft' ? at - 1 : e.key === 'Home' ? 0 : e.key === 'End' ? all.length - 1 : -1;
    if (to < 0 || to >= all.length) return;
    e.preventDefault();
    all[to]!.focus();
  };

  const tile = (t: TileSpec, i: number, isView: boolean) => (
    <button
      key={t.id}
      type="button"
      className="sb-tile"
      data-tile=""
      data-tile-view={isView ? t.id : undefined}
      data-testid={t.testId}
      aria-label={t.badge ? `${t.label}, ${t.badge.label}` : t.label}
      aria-pressed={t.pressed === undefined ? undefined : t.pressed}
      aria-disabled={t.disabled ? 'true' : undefined}
      title={t.title}
      tabIndex={i === rove ? 0 : -1}
      onFocus={() => setFocus(i)}
      onClick={() => activate(t, isView)}
      onKeyDown={(e) => onKeyDown(e, t, isView)}
    >
      <span className="sb-tile-icon" aria-hidden="true">
        {t.icon}
      </span>
      <span className="sb-tile-label">{t.label}</span>
      {t.badge && (
        <span className="sb-tile-badge" aria-hidden="true" data-testid={t.badge.testId}>
          {t.badge.text}
        </span>
      )}
    </button>
  );

  const moreItems: MenuItem[] = hidden.map((t) => ({
    id: t.id,
    label: t.badge ? `${t.title} · ${t.badge.text}` : t.title,
    icon: t.icon,
    checked: t.pressed === true,
    disabled: t.disabled,
    onSelect: () => activate(t, true),
  }));

  return (
    <div ref={row} className="sb-tiles sb-navs" role="toolbar" aria-label="Sidebar" data-testid="sidebar-tiles">
      {lead.map((t, i) => tile(t, i, false))}
      {shown.map((id, i) => tile(byId.get(id)!, lead.length + i, true))}
      {hidden.length > 0 && (
        <button
          type="button"
          className="sb-tile"
          data-tile=""
          data-testid="tile-more"
          aria-label="More"
          aria-haspopup="menu"
          aria-expanded={menu !== null}
          title="More"
          tabIndex={total - 1 === rove ? 0 : -1}
          onFocus={() => setFocus(total - 1)}
          onKeyDown={(e) => onKeyDown(e, null, false)}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setMenu({ x: r.left, y: r.bottom });
          }}
        >
          <span className="sb-tile-icon" aria-hidden="true">
            <IconMore size={20} />
          </span>
          <span className="sb-tile-label">More</span>
        </button>
      )}
      {menu && <ContextMenu x={menu.x} y={menu.y} items={moreItems} label="More views" onClose={() => setMenu(null)} testId="tile-more-menu" />}
    </div>
  );
}
