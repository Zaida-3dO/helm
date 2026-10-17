// The side panel: the Inspector's fixed slots followed by the chat's
// document tabs, with the "+" menu, Maximize and the panel X. Mounted once by the app frame.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent, ReactNode } from 'react';
import type { Api } from '../api/client.ts';
import { panelOps } from '../api/panel.ts';
import { useBinding, useLayer } from '../ui/index.ts';
import { CloseIcon, FileDiffIcon, FolderTreeIcon, MaximizeIcon, PlusIcon, QuestionIcon, RestoreIcon } from './icons.tsx';
import { configureLinks } from './links.ts';
import { entrySlot, SLOTS, slotEntry } from './model.ts';
import type { PanelTab, SlotId } from './model.ts';
import { kindTitle, panelStore, togglePanel } from './registry.ts';
import type { TabContext } from './registry.ts';
import { startTabSync } from './sync.ts';
import { ViewerModal } from './viewer/kinds.tsx';
import { QuickOpen } from './quickopen/QuickOpen.tsx';
import './panel.css';

export const PANEL_MIN_WIDTH = 280;
/** The panel never leaves less than this for the rest of the window. */
export const PANEL_WINDOW_RESERVE = 321;
const NOTICE_MS = 5000;
const DRAG_THRESHOLD_PX = 4;

const SLOT_LABEL: Record<SlotId, string> = { changes: 'Changes', files: 'Files' };
const SLOT_ICON: Record<SlotId, ReactNode> = { changes: <FileDiffIcon />, files: <FolderTreeIcon /> };

function usePanel(): number {
  return useSyncExternalStore(panelStore.subscribe, panelStore.getVersion);
}

function useWindowFocused(): boolean {
  const [focused, setFocused] = useState(() => (typeof document === 'undefined' ? true : document.hasFocus()));
  useEffect(() => {
    const on = () => setFocused(true);
    const off = () => setFocused(false);
    window.addEventListener('focus', on);
    window.addEventListener('blur', off);
    return () => {
      window.removeEventListener('focus', on);
      window.removeEventListener('blur', off);
    };
  }, []);
  return focused;
}

function clampWidth(w: number): number {
  const max = Math.max(PANEL_MIN_WIDTH, window.innerWidth - PANEL_WINDOW_RESERVE);
  return Math.round(Math.min(max, Math.max(PANEL_MIN_WIDTH, w)));
}

export interface SidePanelProps {
  api: Api;
  /** The selected chat (null: none selected). */
  chatId: string | null;
}

export function SidePanel({ api, chatId }: SidePanelProps) {
  usePanel();
  const windowFocused = useWindowFocused();
  const m = panelStore.model;

  // Kinds build their host ops from this; set before any tab body renders.
  panelStore.api = api;
  useEffect(() => panelStore.setCurrent(chatId), [chatId]);
  useEffect(() => startTabSync(panelOps(api)), [api]);
  useEffect(() => {
    configureLinks({
      openExternal: (url) => api('open_external', { url }),
      notify: (text) => panelStore.showNotice(text),
    });
  }, [api]);

  const visible = m.visible(chatId);
  const maximized = m.maximized && visible;

  // Maximised: the panel takes the main area. The frame hides its centre pane on this attribute.
  useEffect(() => {
    const root = document.documentElement;
    if (maximized) root.dataset.panelMaximized = 'true';
    else delete root.dataset.panelMaximized;
    return () => {
      delete root.dataset.panelMaximized;
    };
  }, [maximized]);

  // Ctrl+Shift+I toggles the panel, through the shared key registry.
  useBinding({ id: 'panel.toggle', title: 'Toggle side panel', defaultKeys: 'Ctrl+Shift+I', run: togglePanel });

  // Escape restores a maximised panel (a "panel" layer: menus, popovers and dialogs go first).
  useLayer('panel', maximized, () => panelStore.update((mm) => mm.setMaximized(false)));
  // Escape pressed inside the panel never reaches the composer's Stop: swallowed while focus is in it.
  const asideRef = useRef<HTMLElement>(null);
  useLayer('panel', visible, () => true, () => Boolean(asideRef.current?.contains(document.activeElement)));

  const displayed = m.displayed(chatId);
  const active = m.activeEntry(chatId);
  const activeSlot = active ? entrySlot(active) : null;
  const activeTab = active && !activeSlot ? (displayed.find((t) => t.id === active) ?? null) : null;
  const mounted = activeSlot ? [] : m.mounted(chatId, (k) => Boolean(panelStore.kinds.get(k)?.keepMounted));

  const contextFor = useCallback(
    (tab: PanelTab): TabContext => {
      const isActive = tab.id === active;
      const isVisible = isActive && visible;
      return {
        active: isActive,
        maximized,
        visible: isVisible,
        focused: isVisible && windowFocused,
        intent: tab.intent,
        setTitle: (title) => {
          if (tab.title === title) return;
          tab.title = title;
          panelStore.changed();
        },
        setData: (data) => {
          tab.data = data;
          panelStore.changed();
        },
        markUserIntent: () => panelStore.update((mm) => mm.select(tab.id, panelStore.current)),
      };
    },
    [active, visible, maximized, windowFocused],
  );

  if (!visible) {
    return (
      <>
        <LinkNotice />
        <ViewerModal />
        <QuickOpen chatId={chatId} />
      </>
    );
  }

  const kindOf = (t: PanelTab) => panelStore.kinds.get(t.kind);
  const headerActions = activeTab ? kindOf(activeTab)?.headerActions?.(activeTab, contextFor(activeTab)) : null;

  return (
    <>
      <aside
        className={maximized ? 'side-panel maximized' : 'side-panel'}
        data-testid="side-panel"
        aria-label="Side panel"
        ref={asideRef}
        style={maximized ? undefined : { width: clampWidth(panelStore.width) }}
      >
        {!maximized && <Resizer />}
        <div className="sp-header">
          <TabStrip chatId={chatId} displayed={displayed} active={active} />
          <div className="sp-controls">
            <PlusMenu />
            <button
              type="button"
              className="sp-icon-button"
              title={maximized ? 'Restore (Esc)' : 'Maximize'}
              aria-label={maximized ? 'Restore' : 'Maximize'}
              aria-pressed={maximized}
              onClick={() => panelStore.update((mm) => mm.setMaximized(!mm.maximized))}
            >
              {maximized ? <RestoreIcon /> : <MaximizeIcon />}
            </button>
            <button
              type="button"
              className="sp-icon-button"
              title="Close side panel"
              aria-label="Close side panel"
              onClick={() =>
                panelStore.update((mm) => {
                  mm.hide();
                  mm.setInspector(false);
                })
              }
            >
              <CloseIcon />
            </button>
          </div>
        </div>
        {headerActions ? <div className="sp-actions">{headerActions}</div> : null}
        <div className="sp-body">
          {activeSlot ? (
            <SlotBody slot={activeSlot} chatId={chatId} visible={visible} />
          ) : (
            mounted.map((tab) => (
              <div
                key={tab.id}
                className="sp-tab-body"
                role="tabpanel"
                hidden={tab.id !== active}
                data-testid="panel-tab-body"
                data-kind={tab.kind}
              >
                <TabBody tab={tab} ctx={contextFor(tab)} />
              </div>
            ))
          )}
        </div>
      </aside>
      <LinkNotice />
      <ViewerModal />
      <QuickOpen chatId={chatId} />
    </>
  );
}

function TabBody({ tab, ctx }: { tab: PanelTab; ctx: TabContext }) {
  const kind = panelStore.kinds.get(tab.kind);
  if (!kind) {
    return (
      <div className="sp-empty">
        <QuestionIcon size={25} />
        <div>This tab can&rsquo;t be shown in this version of the app.</div>
      </div>
    );
  }
  return <>{kind.render(tab, ctx)}</>;
}

function SlotBody({ slot, chatId, visible }: { slot: SlotId; chatId: string | null; visible: boolean }) {
  const def = panelStore.slots.get(slot);
  if (def) return <>{def.render({ chatId, visible })}</>;
  return slot === 'changes' ? (
    <div className="sp-empty" data-testid="panel-slot-empty" data-slot="changes">
      <FileDiffIcon size={25} />
      <div className="sp-empty-heading">No changes yet</div>
      <div>Changes will appear here as you work.</div>
    </div>
  ) : (
    <div className="sp-empty" data-testid="panel-slot-empty" data-slot="files">
      <FolderTreeIcon size={25} />
      <div className="sp-empty-heading">No files yet</div>
      <div>Pick a chat with a project folder to browse its files.</div>
    </div>
  );
}

interface Drag {
  id: string;
  startX: number;
  dragging: boolean;
  pointerId: number;
}

function TabStrip({ chatId, displayed, active }: { chatId: string | null; displayed: PanelTab[]; active: string | null }) {
  const m = panelStore.model;
  const entries = useMemo(
    () => [...(m.inspector ? SLOTS.map((s) => slotEntry(s)) : []), ...displayed.map((t) => t.id)],
    [m.inspector, displayed],
  );
  const refs = useRef(new Map<string, HTMLElement>());
  const focusAfter = useRef<string | null>(null);
  const drag = useRef<Drag | null>(null);

  // Keep the selected entry in view in a crowded strip, and move focus after keyboard moves.
  useLayoutEffect(() => {
    if (active) refs.current.get(active)?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
    const want = focusAfter.current;
    if (want) {
      focusAfter.current = null;
      refs.current.get(want)?.focus();
    }
  });

  const selectEntry = (entry: string) => panelStore.update((mm) => mm.select(entry, panelStore.current));

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const focused = (e.target as HTMLElement).closest<HTMLElement>('[data-entry]')?.dataset.entry ?? active;
    if (!focused) return;
    const i = entries.indexOf(focused);
    const tab = displayed.find((t) => t.id === focused);
    if (e.altKey && e.shiftKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight') && tab) {
      e.preventDefault();
      const tabIndex = displayed.indexOf(tab);
      focusAfter.current = tab.id;
      panelStore.update((mm) => mm.reorder(tab.id, tabIndex + (e.key === 'ArrowLeft' ? -1 : 1), chatId));
      return;
    }
    let next: string | undefined;
    if (e.key === 'ArrowLeft') next = entries[Math.max(0, i - 1)];
    else if (e.key === 'ArrowRight') next = entries[Math.min(entries.length - 1, i + 1)];
    else if (e.key === 'Home') next = entries[0];
    else if (e.key === 'End') next = entries[entries.length - 1];
    else if ((e.key === 'Delete' || e.key === 'Backspace') && tab) {
      e.preventDefault();
      const after = entries[i + 1] ?? entries[i - 1] ?? null;
      focusAfter.current = after;
      panelStore.update((mm) => mm.close(tab.id, 'user'));
      return;
    }
    if (next) {
      e.preventDefault();
      focusAfter.current = next;
      selectEntry(next);
    }
  };

  const onPointerDown = (e: ReactPointerEvent<HTMLElement>, id: string) => {
    if (e.button !== 0) return;
    drag.current = { id, startX: e.clientX, dragging: false, pointerId: e.pointerId };
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d || d.pointerId !== e.pointerId) return;
    if (!d.dragging && Math.abs(e.clientX - d.startX) > DRAG_THRESHOLD_PX) {
      d.dragging = true;
      (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    }
  };
  const onPointerUp = (e: ReactPointerEvent<HTMLElement>) => {
    const d = drag.current;
    drag.current = null;
    if (!d?.dragging) return;
    // Drop before the first other tab whose midpoint is right of the pointer.
    const others = displayed.filter((t) => t.id !== d.id);
    let index = others.length;
    for (let k = 0; k < others.length; k++) {
      const el = refs.current.get((others[k] as PanelTab).id);
      if (!el) continue;
      const r = el.getBoundingClientRect();
      if (r.left + r.width / 2 > e.clientX) {
        index = k;
        break;
      }
    }
    panelStore.update((mm) => mm.reorder(d.id, index, chatId));
  };

  const setRef = (id: string) => (el: HTMLElement | null) => {
    if (el) refs.current.set(id, el);
    else refs.current.delete(id);
  };

  return (
    <div className="sp-tabs" role="tablist" aria-label="Side panel tabs" onKeyDown={onKeyDown}>
      {m.inspector &&
        SLOTS.map((slot) => {
          const entry = slotEntry(slot);
          const selected = active === entry;
          const def = panelStore.slots.get(slot);
          return (
            <div
              key={entry}
              ref={setRef(entry)}
              role="tab"
              className="sp-tab sp-slot"
              aria-selected={selected}
              tabIndex={selected ? 0 : -1}
              data-entry={entry}
              data-testid="panel-slot"
              data-slot={slot}
              onClick={() => selectEntry(entry)}
            >
              <span className="sp-tab-content">
                {def?.icon ?? SLOT_ICON[slot]}
                <span className="sp-tab-title">{def?.title ?? SLOT_LABEL[slot]}</span>
              </span>
            </div>
          );
        })}
      {m.inspector && displayed.length > 0 && <span className="sp-divider" aria-hidden />}
      {displayed.map((tab) => {
        const selected = active === tab.id;
        const kind = panelStore.kinds.get(tab.kind);
        const title = kind ? kindTitle(tab) : 'Unknown tab';
        return (
          <div
            key={tab.id}
            ref={setRef(tab.id)}
            role="tab"
            className={tab.intent === 'program' ? 'sp-tab sp-doc program' : 'sp-tab sp-doc'}
            aria-selected={selected}
            tabIndex={selected ? 0 : -1}
            title={title}
            data-entry={tab.id}
            data-testid="panel-tab"
            data-kind={tab.kind}
            data-intent={tab.intent}
            onClick={() => selectEntry(tab.id)}
            onPointerDown={(e) => onPointerDown(e, tab.id)}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={() => (drag.current = null)}
          >
            <span className="sp-tab-content">
              {kind?.icon?.(tab) ?? <QuestionIcon />}
              <span className="sp-tab-title">{title}</span>
            </span>
            <button
              type="button"
              className="sp-tab-close"
              tabIndex={-1}
              aria-label={`Close ${title}`}
              title="Close"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                panelStore.update((mm) => mm.close(tab.id, 'user'));
              }}
            >
              <CloseIcon size={12} />
            </button>
          </div>
        );
      })}
    </div>
  );
}

function PlusMenu() {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  const firstItem = useRef<HTMLButtonElement>(null);
  // Asked on each render: an entry's `enabled()` may change at any time.
  const entries = [...panelStore.kinds.values()].flatMap((k) => (k.menuEntry && k.menuEntry.enabled() ? [k.menuEntry] : []));

  // An open "+" menu owns Escape: it closes only the menu.
  useLayer('menu', open, () => setOpen(false));

  useEffect(() => {
    if (!open) return;
    firstItem.current?.focus();
    const onDown = (e: PointerEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener('pointerdown', onDown, true);
    return () => window.removeEventListener('pointerdown', onDown, true);
  }, [open]);

  if (entries.length === 0) return null;
  return (
    <div
      className="sp-plus"
      ref={wrap}
    >
      <button
        type="button"
        className="sp-icon-button"
        title="Open in side panel"
        aria-label="Open in side panel"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <PlusIcon />
      </button>
      {open && (
        <div className="sp-menu" role="menu">
          {entries.map((entry, i) => (
            <button
              key={entry.label}
              ref={i === 0 ? firstItem : undefined}
              type="button"
              role="menuitem"
              className="sp-menu-item"
              onClick={() => {
                setOpen(false);
                entry.open();
              }}
            >
              {entry.icon}
              <span>{entry.label}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function Resizer() {
  const start = useRef<{ x: number; width: number } | null>(null);
  return (
    <div
      className="sp-resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize side panel"
      onPointerDown={(e) => {
        start.current = { x: e.clientX, width: clampWidth(panelStore.width) };
        e.currentTarget.setPointerCapture?.(e.pointerId);
      }}
      onPointerMove={(e) => {
        const s = start.current;
        if (!s) return;
        panelStore.width = clampWidth(s.width + (s.x - e.clientX));
        panelStore.changed();
      }}
      onPointerUp={() => {
        start.current = null;
      }}
    />
  );
}

/** The link gate's refusal notice. Always mounted, so it shows even while the panel is hidden. */
function LinkNotice() {
  const notice = panelStore.notice;
  useEffect(() => {
    if (!notice) return;
    const id = notice.id;
    const timer = setTimeout(() => panelStore.clearNotice(id), NOTICE_MS);
    return () => clearTimeout(timer);
  }, [notice]);
  return (
    <div className="sp-notice-region" role="status" aria-live="polite">
      {notice ? (
        <div className="sp-notice" data-testid="link-notice" key={notice.id}>
          {notice.text}
        </div>
      ) : null}
    </div>
  );
}
