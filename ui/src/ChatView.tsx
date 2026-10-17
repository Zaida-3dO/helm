// One chat: transcript, activity dock, interaction cards, queue and composer.

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { ChatOps } from './api/ops.ts';
import type { MessageRow } from './api/types.ts';
import { HOME_SEND_BEHAVIOR } from './api/types.ts';
import { Composer, isBusy } from './composer/Composer.tsx';
import type { SendShortcut } from './composer/keymap.ts';
import { movePosition, orderPending, itemState } from './composer/queue.ts';
import { QueueList } from './composer/QueueList.tsx';
import type { Flushers } from './shell/quit.ts';
import type { ChatStore } from './state/store.ts';
import { ActivityDock } from './transcript/ActivityDock.tsx';
import { InteractionCard } from './transcript/InteractionCard.tsx';
import type { LinkTarget } from './transcript/Markdown.tsx';
import type { RowContext } from './transcript/Rows.tsx';
import { Transcript } from './transcript/Transcript.tsx';

export interface ChatViewProps {
  chatId: string;
  store: ChatStore;
  ops: ChatOps;
  flushers: Flushers;
  sendShortcut: SendShortcut;
}

export function ChatView({ chatId, store, ops, flushers, sendShortcut }: ChatViewProps) {
  const view = useSyncExternalStore(store.subscribe, () => store.view(chatId));
  const [notice, setNotice] = useState<string | null>(null);
  const [jumpTo, setJumpTo] = useState<number | null>(null);
  const setTextRef = useRef<(text: string) => void>(() => undefined);

  useEffect(() => store.watch(chatId), [store, chatId]);

  const reload = useCallback(async () => {
    const [{ chat, thread }, page] = await Promise.all([ops.get(chatId), ops.page(chatId)]);
    store.chat(chat);
    store.thread(thread);
    store.page(page, 'replace');
  }, [ops, store, chatId]);

  useEffect(() => {
    void reload().catch((e: unknown) => setNotice(`Could not load the chat: ${String((e as Error).message ?? e)}`));
  }, [reload, view.refreshes]);

  // "Around" jumps: load the page around a row, then scroll to it. Nothing
  // links to an old row yet; the dev console can call window.__helmJump(rowId).
  const jumpAround = useCallback(
    async (rowId: number) => {
      const page = await ops.page(chatId, { around: rowId, inclusive: true });
      store.setKeep(chatId, 'oldest');
      store.page(page, 'replace');
      setJumpTo(rowId);
    },
    [ops, store, chatId],
  );
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    (window as unknown as { __helmJump?: (id: number) => void }).__helmJump = (id) => void jumpAround(id);
  }, [jumpAround]);

  const chat = view.chat;
  const thread = view.thread;
  const behavior = thread?.sendBehavior ?? HOME_SEND_BEHAVIOR;
  const busy = chat ? isBusy(chat.status) : false;

  const stopTool = useCallback(
    (toolCallId: string) => {
      void ops.stopTool(chatId, toolCallId).catch(() => undefined);
    },
    [ops, chatId],
  );
  const ctx: RowContext = useMemo(
    () => ({
      canStopTool: (row: MessageRow) =>
        behavior.toolStop && Boolean(thread?.capabilities.stopTool) && Boolean(row.details?.stoppable) && row.status === 'streaming',
      onStopTool: stopTool,
      onLink: (t: LinkTarget) =>
        setNotice(t.kind === 'url' ? `Links open in a later version: ${t.href ?? ''}` : `Vault links open in a later version: ${t.note ?? ''}`),
    }),
    [behavior.toolStop, thread?.capabilities.stopTool, stopTool],
  );

  // Esc anywhere in the chat stops the turn and lets the queue continue.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || !busy || e.defaultPrevented) return;
      void ops.stop(chatId, true).catch(() => undefined);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [busy, ops, chatId]);

  const onMove = useCallback(
    (delta: -1 | 1) => {
      const q = orderPending(view.pending).filter((s) => itemState(s) === 'queued').at(-1);
      if (!q) return;
      const pos = movePosition(view.pending, q.id, delta);
      if (pos !== null) void ops.submissionAction(q.id, 'move', pos).catch(() => undefined);
    },
    [view.pending, ops],
  );

  if (!chat) return <div className="chat-loading muted">{notice ?? 'Loading the chat…'}</div>;

  const lastUser = [...view.rows].reverse().find((r) => r.role === 'user');
  const turnStartedAt = busy && lastUser ? lastUser.timestamp : null;

  return (
    <div className="chat-view" data-testid="chat-view" data-chat-id={chat.id} data-chat-status={chat.status}>
      <header className="chat-head">
        <h1>{chat.title}</h1>
        <span className="mono muted">{chat.cwd}</span>
        <span className={`status-pill s-${chat.status}`} data-testid="chat-status">
          {chat.status}
        </span>
      </header>
      {notice && (
        <div className="notice" role="status">
          {notice}
          <button type="button" className="small" onClick={() => setNotice(null)}>
            Dismiss
          </button>
        </div>
      )}
      <Transcript
        rows={view.rows}
        live={busy}
        hasOlder={view.hasOlder}
        ctx={ctx}
        jumpToRowId={jumpTo}
        onLoadOlder={async () => {
          if (!view.olderCursor) return;
          store.page(await ops.page(chatId, { before: view.olderCursor }), 'older');
        }}
        onJumpLatest={async () => {
          setJumpTo(null);
          store.setKeep(chatId, 'newest');
          if (view.hasNewer) store.page(await ops.page(chatId), 'replace');
        }}
        onFollowChange={(f) => store.setKeep(chatId, f ? 'newest' : 'oldest')}
      />
      {thread && <ActivityDock chat={chat} thread={thread} turnStartedAt={turnStartedAt} onStopTool={stopTool} />}
      {thread?.interactions.map((i) => (
        <InteractionCard key={i.id} interaction={i} onAnswer={(id, a) => void ops.answer(chatId, id, a).catch(() => undefined)} />
      ))}
      {chat.status === 'interrupted' && (
        <div className="banner" data-testid="interrupted-banner">
          <span>{chat.error ?? 'This chat was interrupted. Nothing saved was lost; check the chat before continuing.'}</span>
          {lastUser && (
            <button type="button" className="small" data-testid="recover-draft" onClick={() => setTextRef.current(lastUser.text)}>
              Recover draft
            </button>
          )}
        </div>
      )}
      <QueueList pending={view.pending} thread={thread} running={busy} ops={ops} chatId={chatId} />
      <Composer
        chat={chat}
        thread={thread}
        pending={view.pending}
        ops={ops}
        flushers={flushers}
        sendShortcut={sendShortcut}
        onMove={onMove}
        registerSetText={(f) => {
          setTextRef.current = f;
        }}
      />
    </div>
  );
}
