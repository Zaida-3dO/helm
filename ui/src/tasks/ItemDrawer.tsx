// The item drawer: one board item's title, state, description and links, an Open chat split button
// (its chats are Helm's own links; nothing is written to the board), and three sections read 20 at
// a time: history and checkpoints, evidence, open loops.

import { useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { itemLinkOps, type ItemLink } from '../api/itemlinks.ts';
import { useOpenItem } from '../itemlinks/ItemUi.tsx';
import { openNewChat } from '../newchat/index.ts';
import { useShell } from '../shell/context.ts';
import { Button, ContextMenu, toast } from '../ui/index.ts';
import { IconChevronDown, IconX } from '../ui/icons.tsx';
import { itemContextBlock } from './context.ts';
import type { Section } from './drawer.ts';
import { safeHttpUrl } from './model.ts';
import { restorePanel } from './open.ts';
import type { TasksStore } from './store.ts';

function when(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
}

/** A link as a link when it is plain http(s); anything else is inert text with its raw value. */
function LinkChip({ label, url }: { label: string; url: string }) {
  const safe = safeHttpUrl(url);
  if (!safe) {
    return (
      <span className="tk-link-inert">
        {label} · {url}
      </span>
    );
  }
  return (
    <a href={safe} target="_blank" rel="noreferrer noopener" className="tk-link">
      {label}
    </a>
  );
}

function SectionView<T>({ title, section, empty, onMore, children }: { title: string; section: Section<T>; empty: string; onMore: () => void; children: (item: T) => ReactNode }) {
  const count = section.total === null ? (section.started ? ` · ${section.items.length} loaded` : '') : ` · ${section.items.length} of ${section.total}`;
  return (
    <section className="tk-section" data-testid="tasks-drawer-section" data-section={title}>
      <h4>
        {title}
        <span className="tk-muted">{count}</span>
      </h4>
      {section.items.map((it, i) => (
        <div className="tk-entry" key={i}>
          {children(it)}
        </div>
      ))}
      {section.started && section.items.length === 0 && !section.loading && !section.error && <div className="tk-muted">{empty}</div>}
      {section.error && (
        <div className="tk-error" role="alert">
          {section.error}
        </div>
      )}
      {(section.nextCursor || section.error) && (
        <Button variant="text" disabled={section.loading} onClick={onMore}>
          {section.loading ? 'Loading…' : section.error ? 'Retry' : 'Load more'}
        </Button>
      )}
      {section.loading && !section.nextCursor && !section.error && <div className="tk-muted">Loading…</div>}
    </section>
  );
}

export function ItemDrawer({ store }: { store: TasksStore }) {
  const drawer = store.drawer;
  const shell = useShell();
  const openItem = useOpenItem();
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [links, setLinks] = useState<ItemLink[] | null>(null);
  const chevron = useRef<HTMLButtonElement>(null);
  const d = useSyncExternalStore(drawer?.subscribe ?? (() => () => undefined), drawer?.snapshot ?? (() => null));

  const id = drawer?.id;
  useEffect(() => {
    if (!id) return;
    let live = true;
    const ops = itemLinkOps(shell.api);
    const read = () =>
      void ops.forItem(id).then(
        (l) => live && setLinks(l),
        () => live && setLinks([]),
      );
    read();
    const off = shell.bus.on('item_links', read);
    return () => {
      live = false;
      off();
    };
  }, [id, shell]);

  if (!drawer || !d) return null;
  const item = d.item;
  const newChatHere = () => {
    const block = itemContextBlock({
      id: d.id,
      title: item?.title ?? 'Task',
      state: item?.state ?? '',
      priority: item?.priority ?? null,
      headline: item?.headline ?? null,
      checkpoint: d.checkpoint,
      links: item?.links ?? [],
      body: item?.body ?? null,
    });
    restorePanel();
    openNewChat({ text: block, itemId: d.id, itemTitle: item?.title });
  };
  const linked = links?.length ?? 0;
  return (
    <aside className="tk-drawer" data-testid="tasks-drawer" data-item-id={d.id} aria-label="Task details">
      <header className="tk-drawer-head">
        <h3>{item?.title ?? 'Task details'}</h3>
        <Button variant="icon" label="Close task details" data-testid="tasks-drawer-close" onClick={() => store.closeDrawer()}>
          <IconX size={14} />
        </Button>
      </header>
      {d.loading && <div className="tk-muted">Loading task…</div>}
      {d.error && (
        <div className="tk-error" role="alert">
          {d.error}{' '}
          <Button variant="text" onClick={() => void drawer.loadItem()}>
            Retry
          </Button>
        </div>
      )}
      {item && (
        <>
          <div className="tk-muted">
            {item.state}
            {item.priority ? ` · ${item.priority}` : ''}
          </div>
          <div className="tk-actions">
            <div className="tk-split">
              <Button
                variant="secondary"
                data-testid="tasks-open-chat"
                disabled={links !== null && linked === 0}
                title={linked === 0 ? 'No chat is linked to this task yet' : 'Open the main chat for this task'}
                onClick={() => {
                  restorePanel();
                  openItem(d.id);
                }}
              >
                Open chat{linked > 1 ? ` (${linked})` : ''}
              </Button>
              <button
                ref={chevron}
                type="button"
                className="tk-split-more"
                aria-label="More ways to open a chat"
                data-testid="tasks-chat-menu"
                onClick={() => {
                  const r = chevron.current?.getBoundingClientRect();
                  setMenu({ x: r?.left ?? 0, y: (r?.bottom ?? 0) + 2 });
                }}
              >
                <IconChevronDown size={12} />
              </button>
            </div>
          </div>
          <pre className="tk-body-text" data-testid="tasks-drawer-body">
            {item.body?.trim() ? item.body : 'No description'}
          </pre>
          {(item.links ?? []).length > 0 && (
            <div className="tk-links">
              {(item.links ?? []).map((l, i) => (
                <LinkChip key={`${l.key}${i}`} label={l.key} url={l.url} />
              ))}
            </div>
          )}
          <SectionView title="History and checkpoints" section={d.history} empty="No entries" onMore={() => void drawer.loadSection('history')}>
            {(e) => (
              <>
                <div>{e.headline?.trim() || e.type}</div>
                <div className="tk-muted">{when(e.ts)}</div>
                {e.body && <div className="tk-entry-body">{e.body}</div>}
              </>
            )}
          </SectionView>
          <SectionView title="Evidence" section={d.evidence} empty="No entries" onMore={() => void drawer.loadSection('evidence')}>
            {(a) => (
              <>
                <div>
                  {a.kind}
                  {a.verdict ? ` · ${a.verdict}` : ''}
                </div>
                {(a.body ?? a.bodyPreview) && <div className="tk-entry-body">{a.body ?? a.bodyPreview}</div>}
                {a.bodyTruncated && <div className="tk-muted">Preview only</div>}
                {a.ref && <LinkChip label="Open reference" url={a.ref} />}
              </>
            )}
          </SectionView>
          <SectionView title="Open loops" section={d.loops} empty="No entries" onMore={() => void drawer.loadSection('loops')}>
            {(l) => (
              <>
                <div className="tk-muted">
                  {l.kind} · {when(l.openedAt)}
                </div>
                <div className="tk-entry-body">{d.loopText[l.loopId] ?? l.text}</div>
                {l.textTruncated && d.loopText[l.loopId] === undefined && (
                  <Button variant="text" onClick={() => void drawer.loadLoopText(l.loopId)}>
                    Load full text
                  </Button>
                )}
              </>
            )}
          </SectionView>
        </>
      )}
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          label="Open chat"
          testId="tasks-chat-menu-list"
          onClose={() => setMenu(null)}
          items={[
            {
              id: 'new',
              label: 'New chat in this task',
              onSelect: () => {
                if (!item) {
                  toast.show({ kind: 'info', text: 'Wait for the task to load first.' });
                  return;
                }
                newChatHere();
              },
            },
          ]}
        />
      )}
    </aside>
  );
}
