// One chat: transcript, interaction cards, queue, activity dock and composer.

import { flushSync } from 'react-dom';
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { ChatOps } from './api/ops.ts';
import type { MessageRow } from './api/types.ts';
import { HOME_SEND_BEHAVIOR } from './api/types.ts';
import { FlagNote } from './attention/index.ts';
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
import type { EditState } from './transcript/EditMessage.tsx';
import { forkAction } from './transcript/forkAction.ts';
import { registerMessageAction } from './transcript/slots.ts';
import { useShell } from './shell/context.ts';
import { useLayer } from './ui/index.ts';
import { IconFork } from './ui/icons.tsx';
import { accessoriesAt } from './composer/slots.ts';
import { recallPrompts } from './composer/recall.ts';
import { SelectionToolbar } from './transcript/SelectionToolbar.tsx';
import { FindBar } from './transcript/FindBar.tsx';
import { TimelineRail } from './transcript/TimelineRail.tsx';
import { useWholeChat } from './transcript/useWholeChat.ts';
import { setLocalImageLoader } from './markdown/localImages.ts';
import { registerSelectionAction } from './transcript/slots.ts';
import { handleLink } from './panel/links.ts';
import { openInPanel } from './panel/registry.ts';
import { SIDE_CHAT_KIND, setSideChatOps } from './sidechat/index.tsx';
import { sideChatAnchor } from './sidechat/anchor.ts';
// Settings → Chat registers itself.
import './settings/chat/index.tsx';
import { parseChatElements, type ChatElements } from './settings/chat/elements.ts';

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
  const [editing, setEditing] = useState<EditState | null>(null);
  const setTextRef = useRef<(text: string) => void>(() => undefined);

  useEffect(() => store.watch(chatId), [store, chatId]);
  const shell = useShell();
  // Pictures on disk named in replies and tool cards come from the host.
  useEffect(() => setLocalImageLoader((path) => shell.api<{ mime: string; bytes: string }>('local_image_get', { path })), [shell]);
  // Transcript element toggles, read once and followed on change.
  const [elements, setElements] = useState<ChatElements>({});
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'chatElements' })
      .then((v) => setElements(parseChatElements(v)))
      .catch(() => undefined);
    return shell.bus.on('preferences', (e) => {
      const changed = (e.changed ?? {}) as Record<string, unknown>;
      if ('chatElements' in changed) setElements(parseChatElements(changed.chatElements));
    });
  }, [shell]);
  useEffect(
    () =>
      registerMessageAction({
        ...forkAction(ops, (id) => shell.select(id), (m) => setNotice(`Could not fork: ${m}`)),
        icon: IconFork,
        order: 10,
      }),
    [ops, shell],
  );

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

  // Opening a chat reads it: the mark it shows is cleared with its own
  // token, and a finished turn's completion is acknowledged once the chat is not running. A mark
  // made while the chat is open stays; a completion arriving while the window is in
  // front is read at once.
  const cleared = useRef<string | null>(null);
  const unreadToken = chat?.unread.markToken ?? null;
  const completionId = chat?.unread.completionId ?? null;
  useEffect(() => {
    if (!unreadToken || cleared.current !== null) return;
    cleared.current = unreadToken;
    void ops.clearUnread(chatId, unreadToken).catch(() => undefined);
  }, [unreadToken, ops, chatId]);
  const loaded = chat !== null;
  useEffect(() => {
    // Loaded without a mark: any mark made from now on is one the user made while here.
    if (loaded && unreadToken === null && cleared.current === null) cleared.current = '';
  }, [loaded, unreadToken]);
  const status = chat?.status;
  useEffect(() => {
    if (!completionId || !status || isBusy(status)) return;
    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
    void ops.ackCompletion(chatId, completionId).catch(() => undefined);
  }, [completionId, status, ops, chatId]);
  const behavior = thread?.sendBehavior ?? HOME_SEND_BEHAVIOR;
  const busy = chat ? isBusy(chat.status) : false;

  const stopTool = useCallback(
    (toolCallId: string) => {
      void ops.stopTool(chatId, toolCallId).catch(() => undefined);
    },
    [ops, chatId],
  );
  const latestAssistantId = useMemo(() => [...view.rows].reverse().find((r) => r.role === 'assistant')?.id ?? null, [view.rows]);
  const interactionOpen = (thread?.interactions.length ?? 0) > 0;
  const ctx: RowContext = useMemo(
    () => ({
      chatId,
      latestAssistantId,
      running: busy,
      interactionOpen,
      elements,
      editing,
      onEdit: (row: MessageRow) => {
        ops
          .editOpen(chatId, row.id)
          .then((o) => setEditing({ rowId: o.messageId, revision: o.revision, text: o.text }))
          .catch((e: unknown) => setNotice(e instanceof Error ? e.message : String(e)));
      },
      onResend: async (text: string) => {
        if (!editing) return;
        await ops.resend(chatId, editing.rowId, editing.revision, text);
        setEditing(null);
      },
      onCancelEdit: () => setEditing(null),
      canStopTool: (row: MessageRow) =>
        behavior.toolStop && Boolean(thread?.capabilities.stopTool) && Boolean(row.details?.stoppable) && row.status === 'streaming',
      onStopTool: stopTool,
      // Every link click goes through the panel's gate, which validates it before acting and
      // shows any refusal in the shell's toast slot itself.
      onLink: (t: LinkTarget) => void handleLink(t, { chatId }),
    }),
    [behavior.toolStop, thread?.capabilities.stopTool, stopTool, chatId, latestAssistantId, busy, interactionOpen, editing, ops, elements],
  );

  // Escape while the chat works stops the turn and lets the queue continue. It is the lowest
  // layer of the shell's Escape stack (menus, popovers, search, dialogs and panels go first).
  useLayer(
    'composer',
    busy,
    () => {
      void ops.stop(chatId, true).catch(() => undefined);
    },
    () => busy,
  );

  const transcriptBox = useRef<HTMLDivElement>(null);
  // Ctrl+A then Ctrl+C copies the whole chat, not just the rows that are mounted.
  const wholeChat = useWholeChat(transcriptBox, (before) => ops.page(chatId, before ? { before } : {}), (text) => setNotice(text));
  // Find in chat: the host searches the whole history; a match in a row that is not
  // loaded loads the page around it first.
  const hostFind = useCallback((q: string, activity: boolean, after?: string) => ops.find(chatId, q, activity, after), [ops, chatId]);
  const loadedIds = useRef(new Set<number>());
  loadedIds.current = new Set(view.rows.map((r) => r.id));
  const jumpToFound = useCallback(
    (rowId: number) => {
      if (loadedIds.current.has(rowId)) setJumpTo(rowId);
      else void jumpAround(rowId).catch(() => undefined);
    },
    [jumpAround],
  );
  // The jump is an effect on the target, so the same target twice does nothing; clear it first.
  const jumpFromRail = useCallback(
    (rowId: number) => {
      flushSync(() => setJumpTo(null));
      setJumpTo(rowId);
    },
    [],
  );
  const [autocompleteOn, setAutocompleteOn] = useState(false);
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'autocomplete' })
      .then((v) => setAutocompleteOn(Boolean(v && typeof v === 'object' && (v as { enabled?: unknown }).enabled === true)))
      .catch(() => undefined);
    return shell.bus.on('preferences', (e) => {
      const c = (e.changed ?? {}) as Record<string, unknown>;
      if ('autocomplete' in c) setAutocompleteOn(Boolean((c.autocomplete as { enabled?: unknown } | null)?.enabled === true));
    });
  }, [shell]);

  // Side chats: the selection toolbar's "Side chat" forks a hidden chat at the reply
  // and opens it in the side panel. Not offered inside a side chat.
  const rowsRef = useRef(view.rows);
  rowsRef.current = view.rows;
  useEffect(() => setSideChatOps(ops), [ops]);
  useEffect(() => {
    if (view.chat?.sideSourceId) return;
    return registerSelectionAction({
      id: 'side-chat',
      label: 'Side chat',
      appliesTo: (sel) => sel.chatId === chatId && sideChatAnchor(rowsRef.current, sel.rowIds) !== null,
      run: (sel) => {
        const anchor = sideChatAnchor(rowsRef.current, sel.rowIds);
        if (anchor === null) return;
        ops
          .fork(chatId, { messageId: anchor, side: true, excerpt: sel.text })
          .then((c) => {
            if (!openInPanel(SIDE_CHAT_KIND, chatId, { chatId: c.id, sourceId: chatId }, { intent: 'user', chatId })) {
              setNotice('The side panel has no room for the side chat.');
            }
          })
          .catch((e: unknown) => setNotice(`Could not open a side chat: ${e instanceof Error ? e.message : String(e)}`));
      },
    });
  }, [ops, chatId, view.chat?.sideSourceId]);

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
      {/* The title and folder live in the top bar and tags row, so the chat draws no
          header of its own. The status stays as text for assistive tech and the e2e hook. */}
      <span className="visually-hidden" data-testid="chat-status">
        {chat.status}
      </span>
      {notice && (
        <div className="notice" role="status">
          {notice}
          <button type="button" className="small" onClick={() => setNotice(null)}>
            Dismiss
          </button>
        </div>
      )}
      <div className="transcript-box" ref={transcriptBox} data-all-selected={wholeChat ? 'true' : undefined}>
      <SelectionToolbar chatId={chatId} container={transcriptBox} blocked={interactionOpen} />
      <FindBar rows={view.rows} hostFind={hostFind} container={transcriptBox} onJump={jumpToFound} onClose={() => setJumpTo(null)} />
      {elements.timeline !== 'hidden' && <TimelineRail rows={view.rows} onJump={jumpFromRail} container={transcriptBox} />}
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
      </div>
      {thread?.interactions.map((i) => (
        <InteractionCard key={i.id} interaction={i} onAnswer={(id, a) => void ops.answer(chatId, id, a).catch(() => undefined)} />
      ))}
      {/* The composer dock: banners, then other areas' dock items (the
          review tray), then the queue, in one capped, scrolling live region above the box. */}
      <div className="composer-dock" role="region" aria-label="Composer" aria-live="polite" data-testid="composer-dock">
      <div className="composer-dock-stack">
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
      {accessoriesAt('consent').map((a) => (
        <div key={a.id} className="composer-accessory" data-accessory={a.id}>
          {a.render({ chat, thread, busy, draftEmpty: !chat.draft })}
        </div>
      ))}
      {accessoriesAt('dock').map((a) => (
        <div key={a.id} className="composer-accessory" data-accessory={a.id}>
          {a.render({ chat, thread, busy, draftEmpty: !chat.draft })}
        </div>
      ))}
      {!chat.sideSourceId && <FlagNote chatId={chatId} ops={ops} />}
      <QueueList pending={view.pending} thread={thread} running={busy} ops={ops} chatId={chatId} />
      </div>
      {/* The activity line sits directly on the box, below the waiting and queued messages. */}
      {thread && <ActivityDock chat={chat} thread={thread} turnStartedAt={turnStartedAt} onStopTool={stopTool} rows={view.hasNewer ? undefined : view.rows} />}
      <Composer
        recall={recallPrompts(view.rows)}
        autocomplete={autocompleteOn}
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
    </div>
  );
}
