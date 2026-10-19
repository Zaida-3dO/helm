// The top-bar action row: one-click buttons from the user's set, and
// a menu for the ones placed in the overflow. A click sends the button's text as an action (the
// draft is left alone); Helm runs nothing itself.
//
// The buttons are quiet ghost buttons with an icon and a label. When the bar is too
// narrow for them and a readable title (topbar/fold.ts), the row folds into a single "Actions"
// menu holding every button. The overflow trigger carries the zap icon, not "…", so it no longer
// looks like the chat menu beside it.

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ComponentType } from 'react';
import { actionOps, type ActionButton, type ActionSet } from '../api/actions.ts';
import { useShell } from '../shell/context.ts';
import { shouldFold, titleRoom } from '../topbar/fold.ts';
import { Button, ContextMenu, type MenuItem } from '../ui/index.ts';
import { IconActions, IconChevronDown, IconHand, IconInfo, IconNewChat, IconPower, IconRefresh, IconRocket, type IconProps } from '../ui/icons.tsx';
import { onActionsChanged } from './changes.ts';
import { overflowButtons, topbarButtons } from './model.ts';
import { ActionError, runButton } from './send.ts';
import './actions.css';

export interface ActionRowProps {
  /** The chat in view, or null. Prompt buttons need one. */
  chatId: string | null;
}

/** Icons for the seeded buttons; a button the user made gets the generic action icon. */
const SEED_ICONS: Record<string, ComponentType<IconProps>> = {
  update: IconInfo,
  'needs-me': IconHand,
  'ship-it': IconRocket,
  'close-session': IconPower,
  'new-session': IconNewChat,
  'git-sync': IconRefresh,
};

function iconFor(b: Pick<ActionButton, 'seedId'>): ComponentType<IconProps> {
  return (b.seedId && SEED_ICONS[b.seedId]) || IconActions;
}

export function ActionRow({ chatId }: ActionRowProps) {
  const shell = useShell();
  const api = shell.api;
  const onSelectChat = useCallback((id: string) => shell.select(id), [shell]);
  const [set, setSet] = useState<ActionSet | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [sending, setSending] = useState<string | null>(null);
  const moreRef = useRef<HTMLButtonElement>(null);
  const rowRef = useRef<HTMLDivElement>(null);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);
  const [folded, setFolded] = useState(false);
  const widths = useRef({ inline: 0, folded: 0 });

  const load = useCallback(() => {
    actionOps(api)
      .get()
      .then(setSet)
      .catch((e: unknown) => setNotice(`Could not load the buttons: ${String((e as Error).message ?? e)}`));
  }, [api]);
  useEffect(() => {
    load();
    return onActionsChanged(load);
  }, [load]);

  const run = useCallback(
    async (id: string) => {
      const button = set?.buttons.find((b) => b.id === id);
      if (!button || sending) return;
      setMenuAt(null);
      setNotice(null);
      setSending(id);
      try {
        await runButton(api, button, { chatId, select: onSelectChat });
      } catch (e) {
        setNotice(e instanceof ActionError ? e.message : String((e as Error).message ?? e));
      } finally {
        setSending(null);
      }
    },
    [api, chatId, onSelectChat, set, sending],
  );

  // A new button set is measured unfolded first (a layout effect, so the unfolded pass never paints).
  useLayoutEffect(() => {
    widths.current.inline = 0;
    setFolded(false);
  }, [set]);

  // Fold or unfold as the bar's room changes.
  useLayoutEffect(() => {
    const row = rowRef.current;
    const bar = row?.closest<HTMLElement>('.topbar');
    const crumbs = bar?.querySelector<HTMLElement>('.crumbs');
    if (!row || !bar || !crumbs) return;
    const check = () => {
      const w = row.getBoundingClientRect().width;
      if (folded) widths.current.folded = w;
      else widths.current.inline = w;
      const room = titleRoom(crumbs.getBoundingClientRect().width, bar.scrollWidth, bar.clientWidth);
      const next = shouldFold(folded, room, widths.current.inline, widths.current.folded);
      if (next !== folded) {
        setMenuAt(null);
        setFolded(next);
      }
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(bar);
    if (row.parentElement) ro.observe(row.parentElement);
    return () => ro.disconnect();
  }, [folded, set]);

  if (!set) return notice ? <div className="action-row-notice error">{notice}</div> : null;
  const top = topbarButtons(set);
  const over = overflowButtons(set);
  const needsChat = (kind: string) => kind === 'prompt' && !chatId;

  const item = (b: ActionButton): MenuItem => {
    const Glyph = iconFor(b);
    return {
      id: b.seedId ?? b.id,
      label: b.label,
      icon: <Glyph />,
      disabled: needsChat(b.kind) || sending !== null,
      onSelect: () => void run(b.id),
    };
  };
  const menuItems: MenuItem[] = folded
    ? [...top.map(item), ...over.map((b, i) => ({ ...item(b), separator: i === 0 && top.length > 0 }))]
    : over.map(item);
  const toggleMenu = () => {
    const r = moreRef.current?.getBoundingClientRect();
    setMenuAt(menuAt ? null : { x: r ? r.right - 194 : 0, y: r ? r.bottom + 4 : 0 });
  };

  return (
    <div ref={rowRef} className="action-row" data-testid="action-row" data-folded={folded ? 'true' : undefined}>
      {!folded &&
        top.map((b) => {
          const Glyph = iconFor(b);
          return (
            <Button
              key={b.id}
              variant="ghost"
              className="action-button"
              data-testid="action-button"
              data-action={b.seedId ?? b.id}
              title={b.text}
              disabled={needsChat(b.kind) || sending !== null}
              onClick={() => void run(b.id)}
            >
              <Glyph />
              <span>{b.label}</span>
            </Button>
          );
        })}
      {folded && top.length + over.length > 0 && (
        <Button
          ref={moreRef}
          variant="ghost"
          className="action-button action-fold"
          data-testid="action-overflow"
          aria-haspopup="menu"
          aria-expanded={menuAt !== null}
          title="Actions"
          onClick={toggleMenu}
        >
          <IconActions />
          <span>Actions</span>
          <IconChevronDown size={12} />
        </Button>
      )}
      {!folded && over.length > 0 && (
        <Button
          ref={moreRef}
          variant="icon"
          className="action-more"
          data-testid="action-overflow"
          aria-haspopup="menu"
          aria-expanded={menuAt !== null}
          label="More actions"
          onClick={toggleMenu}
        >
          <IconActions size={15} />
        </Button>
      )}
      {menuAt && (
        <ContextMenu x={menuAt.x} y={menuAt.y} label={folded ? 'Actions' : 'More actions'} testId="action-menu" onClose={() => setMenuAt(null)} items={menuItems} />
      )}
      {notice && (
        <span className="action-row-notice error" role="status">
          {notice}
        </span>
      )}
    </div>
  );
}
