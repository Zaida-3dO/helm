// The item-link pieces the sidebar draws: the small item chip on a chat row, the item
// sub-folder row, and the picker that opens when an item has several chats.

import { useEffect, useSyncExternalStore } from 'react';
import { itemLinkOps } from '../api/itemlinks.ts';
import { useShell } from '../shell/context.ts';
import { Button, Dialog, toast } from '../ui/index.ts';
import { IconBoard } from '../ui/icons.tsx';
import { itemMarkLabel, type ItemRef } from './folders.ts';
import { itemLinksStoreFor, itemPicker, openItem } from './store.ts';
import './itemlinks.css';

/** The item refs for the chats the sidebar lists. */
export function useItemRefs(chatIds: readonly string[]): ReadonlyMap<string, ItemRef> {
  const shell = useShell();
  const store = itemLinksStoreFor(shell.api, shell.bus);
  useEffect(() => {
    store.start();
    return () => store.stop();
  }, [store]);
  const key = chatIds.join(',');
  useEffect(() => store.setChats(chatIds), [store, key]); // eslint-disable-line react-hooks/exhaustive-deps
  return useSyncExternalStore(store.subscribe, () => store.snapshot);
}

/** Opens an item by id: its main chat, or the picker when it has several. */
export function useOpenItem() {
  const shell = useShell();
  return (itemId: string) =>
    void openItem(shell.api, (id) => shell.select(id), itemId).then((err) => {
      if (err) toast.show({ kind: 'error', text: err });
    });
}

/**
 * A small chip on a chat row naming its item (its title, or a board glyph while the title is
 * unknown); a click opens the item (its main chat). While the row is hovered the row's own
 * "Open linked item" button (`ItemOpenButton`) stands in for it.
 */
export function ItemChip({ item }: { item: ItemRef }) {
  const open = useOpenItem();
  return (
    <span
      className="sb-item-chip"
      role="button"
      tabIndex={-1}
      data-testid="item-chip"
      data-item-id={item.itemId}
      data-named={item.named}
      aria-label={itemMarkLabel(item)}
      title={itemMarkLabel(item)}
      onClick={(e) => {
        e.stopPropagation();
        open(item.itemId);
      }}
      onPointerDown={(e) => e.stopPropagation()}
    >
      {!item.named ? <IconBoard size={11} /> : item.title.length > 14 ? `${item.title.slice(0, 13)}…` : item.title}
    </span>
  );
}

/** The hover button that does what the chip does, so the item stays one click away on a hovered row. */
export function ItemOpenButton({ item }: { item: ItemRef }) {
  const open = useOpenItem();
  const label = itemMarkLabel(item);
  return (
    <button type="button" className="sb-act" data-testid="item-open-button" aria-label={label} title={label} onClick={() => open(item.itemId)}>
      <IconBoard size={14} />
    </button>
  );
}

/** The sub-folder row that holds an item's chats. */
export function ItemFolderRow({
  itemId,
  title,
  members,
  collapsed,
  indent,
  onToggle,
}: {
  itemId: string;
  title: string;
  members: number;
  collapsed: boolean;
  indent: number;
  onToggle: () => void;
}) {
  const open = useOpenItem();
  return (
    <div className="sb-item-folder" data-testid="item-folder" data-item-id={itemId} style={{ marginLeft: indent }}>
      <button type="button" className="sb-item-folder-main" aria-expanded={!collapsed} onClick={onToggle} title={title}>
        <span className="sb-item-caret" aria-hidden="true">
          {collapsed ? '›' : '⌄'}
        </span>
        <span className="sb-item-folder-title">{title}</span>
        <span className="sb-item-count">{members}</span>
      </button>
      <button type="button" className="sb-act sb-item-open" aria-label={`Open ${title}`} title="Open the item's main chat" onClick={() => open(itemId)}>
        ↗
      </button>
    </div>
  );
}

/** The picker for an item with several chats: its main chat first. */
export function ItemPickerHost() {
  const shell = useShell();
  const cur = useSyncExternalStore(itemPicker.subscribe, () => itemPicker.current);
  if (!cur) return null;
  const close = () => itemPicker.show(null);
  const pick = (chatId: string) => {
    close();
    shell.select(chatId);
  };
  const ops = itemLinkOps(shell.api);
  return (
    <Dialog title={cur.itemTitle?.trim() || 'Linked item'} onClose={close} testId="item-picker" intro="This item has several chats. Pick one to open.">
      <ul className="item-picker-list">
        {cur.candidates.map((c) => (
          <li key={c.chatId}>
            <button type="button" className="item-picker-row" data-testid="item-picker-row" data-chat-id={c.chatId} data-main={c.isMain ? 'true' : undefined} onClick={() => pick(c.chatId)}>
              <span className="item-picker-title">{c.title || 'Untitled chat'}</span>
              {c.isMain && <span className="item-picker-tag">Main</span>}
              {c.archived && <span className="item-picker-tag">Archived</span>}
            </button>
            {!c.isMain && (
              <Button
                variant="text"
                onClick={() =>
                  void ops.setMain(cur.itemId, c.chatId).then(
                    () => close(),
                    (e) => toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) }),
                  )
                }
              >
                Make main
              </Button>
            )}
          </li>
        ))}
      </ul>
    </Dialog>
  );
}
