// The Needs attention inbox: a modal over the
// host's attention summary. Each card has Open (opens the chat; the card stays until you act on
// it), Snooze (a menu of times) and Mark addressed. Both actions hide the chat until something new
// happens in it; the host publishes a fresh summary, which this dialog re-renders from.

import { useState } from 'react';
import type { AttentionSummary } from '../api/types.ts';
import { Button, ContextMenu, Dialog, toast, type MenuItem } from '../ui/index.ts';
import { IconCheck, IconClock, IconExternalLink, IconFlag } from '../ui/icons.tsx';
import { groupLabel, inboxRows, snoozeChoices, type InboxRow } from './model.ts';
import './attention.css';

export const ATTENTION_DIALOG = 'attention';

export interface AttentionActions {
  markAddressed: (chatId: string) => Promise<unknown>;
  snooze: (chatId: string, until: number | null) => Promise<unknown>;
}

export interface AttentionInboxProps {
  summary: AttentionSummary | null;
  ops: AttentionActions;
  onOpen: (chatId: string) => void;
  onClose: () => void;
}

export function AttentionInbox({ summary, ops, onOpen, onClose }: AttentionInboxProps) {
  const rows = inboxRows(summary);
  const [busy, setBusy] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; row: InboxRow } | null>(null);

  const act = async (chatId: string, f: () => Promise<unknown>) => {
    setBusy(chatId);
    try {
      await f();
    } catch (e) {
      toast.show({ kind: 'error', text: `Could not update this chat: ${String((e as Error)?.message ?? e)}` });
    } finally {
      setBusy(null);
    }
  };

  const snoozeItems = (row: InboxRow): MenuItem[] =>
    snoozeChoices(new Date()).map((c) => ({
      id: `snooze:${c.id}`,
      label: c.label,
      shortcut: c.when,
      onSelect: () => void act(row.chatId, () => ops.snooze(row.chatId, c.until)),
    }));

  return (
    <Dialog
      title="Needs attention"
      onClose={onClose}
      wide
      testId="attention-inbox"
      intro="Chats waiting on you, chats that may be stuck, and results you have not read. Mark addressed or snooze one to set it aside until something new happens in it."
    >
      <div className="att-body">
        {!summary && <p className="att-note">Checking chats…</p>}
        {summary && rows.length === 0 && (
          <div className="att-empty">
            <IconCheck size={20} />
            <strong>Nothing needs you</strong>
            <span>Chats that wait on you, stall or finish unread appear here.</span>
          </div>
        )}
        <div className="att-cards">
          {rows.map((r) => (
            <article
              key={r.chatId}
              className="att-card"
              data-testid="attention-card"
              data-chat-id={r.chatId}
              data-group={r.group}
              aria-busy={busy === r.chatId ? true : undefined}
            >
              <header className="att-card-head">
                <span className="att-card-title">{r.title || 'Untitled'}</span>
                <span className="att-card-kind">
                  <IconFlag size={11} filled />
                  {groupLabel(r.group)}
                </span>
              </header>
              <div className="att-card-project">{r.project}</div>
              <p className="att-card-reason">{r.reason}</p>
              <div className="att-card-actions">
                <Button
                  variant="secondary"
                  onClick={() => {
                    onOpen(r.chatId);
                    onClose();
                  }}
                >
                  <IconExternalLink size={13} />
                  Open
                </Button>
                <Button
                  variant="ghost"
                  aria-haspopup="menu"
                  disabled={busy === r.chatId}
                  onClick={(e) => {
                    const b = e.currentTarget.getBoundingClientRect();
                    setMenu({ x: b.left, y: b.bottom + 4, row: r });
                  }}
                >
                  <IconClock size={13} />
                  Snooze
                </Button>
                <Button variant="ghost" disabled={busy === r.chatId} onClick={() => void act(r.chatId, () => ops.markAddressed(r.chatId))}>
                  <IconCheck size={13} />
                  Mark addressed
                </Button>
              </div>
            </article>
          ))}
        </div>
        <p className="att-foot">
          Mark addressed or Snooze takes a chat out of this list until something new happens in it. The conversation and its unread
          state are not changed.
        </p>
      </div>
      {menu && (
        <ContextMenu x={menu.x} y={menu.y} items={snoozeItems(menu.row)} label={`Snooze ${menu.row.title}`} onClose={() => setMenu(null)} testId="attention-snooze-menu" />
      )}
    </Dialog>
  );
}
