// One chat: transcript, interaction cards, in-flight messages, activity dock and composer.

import { flushSync } from 'react-dom';
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { ChatOps } from './api/ops.ts';
import { remoteChatOps } from './api/devices.ts';
import type { MessageRow } from './api/types.ts';
import { HOME_SEND_BEHAVIOR } from './api/types.ts';
import { FlagNote } from './attention/index.ts';
import { Composer, isBusy } from './composer/Composer.tsx';
import { useRemoteChat } from './devices/hooks.ts';
import { RemoteChatBar } from './devices/RemoteChatBar.tsx';
import { deviceOps } from './devices/api.ts';
import { forwardLine } from './settings/devices/model.ts';
import { autoOpenPicks, type ForwardRef } from './transcript/cards.ts';
import type { SendShortcut } from './composer/keymap.ts';
import { claudeHold, holdLine, waitReason } from './composer/inFlight.ts';
import { QueueList } from './composer/QueueList.tsx';
import { LOCAL_WORKING_LABEL, isLocalModelId, localWaitLabel } from './state/localModel.ts';
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
import { ConfirmDialog, toast, useLayer, type ConfirmSpec } from './ui/index.ts';
import { rollOverChat, rollOverConfirm } from './sidebar/actions.ts';
import { patchCardOpen } from './sidebar/state.ts';
import { ContextBar } from './transcript/ContextBar.tsx';
import { isTooLargeRefusal, type ChatContextReading } from './transcript/rowMeta.ts';
import { isSignInError } from './transcript/signin.ts';
import { SignInAgain } from './transcript/SignInAgain.tsx';
import { requestPane } from './settings/registry.ts';
import { SETTINGS_DIALOG } from './settings/SettingsDialog.tsx';
import { IconFork } from './ui/icons.tsx';
import { accessoriesAt, composerApi, onComposerSent } from './composer/slots.ts';
import { recallPrompts } from './composer/recall.ts';
import { SelectionToolbar } from './transcript/SelectionToolbar.tsx';
import { useMentionRepo } from './pr/mentionRepo.ts';
import { FindBar } from './transcript/FindBar.tsx';
import { stackFoundToHost } from './transcript/find.ts';
import { TimelineRail } from './transcript/TimelineRail.tsx';
import { useWholeChat } from './transcript/useWholeChat.ts';
import { setLocalImageLoader } from './markdown/localImages.ts';
import { registerSelectionAction } from './transcript/slots.ts';
import { handleLink } from './panel/links.ts';
import { openInPanel } from './panel/registry.ts';
import { mcpOps } from './api/mcp.ts';
import { chatMcpNoticeText } from './settings/mcp/readiness.ts';
import { SIDE_CHAT_KIND, setSideChatOps } from './sidechat/index.tsx';
import { sideChatAnchor } from './sidechat/anchor.ts';
import { anchorSession } from './transcript/session.ts';
// Settings → Chat registers itself.
import './settings/chat/index.tsx';
import { parseChatElements, type ChatElements } from './settings/chat/elements.ts';

/** A turn that died because Claude Code itself could not be found: the worker's line reads
 * "Claude Code executable not found: …", so the way out is Settings → Runtime. */
function isExecutableNotFound(error: string | null | undefined): boolean {
  return !!error && error.toLowerCase().includes('executable not found');
}

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
  const [mcpNotice, setMcpNotice] = useState<string | null>(null);
  const [jumpTo, setJumpTo] = useState<number | null>(null);
  const [editing, setEditing] = useState<EditState | null>(null);
  const [rollConfirm, setRollConfirm] = useState<ConfirmSpec | null>(null);
  const setTextRef = useRef<(text: string) => void>(() => undefined);

  useEffect(() => store.watch(chatId), [store, chatId]);
  const shell = useShell();
  // A chat on another device is read from this PC's copy; what only the host can do travels
  // there as a device-to-device call, answered by the same functions the local UI uses.
  const remote = useRemoteChat(shell.api, shell.bus, view.chat);
  const devOps = useMemo(() => remoteChatOps(shell.api), [shell]);
  // What an agent asked to show here acts by itself only for rows that arrive live while the chat is
  // in view: newer than every row the last full read held (`readUpTo`; nothing acts before the
  // first read), and once per row. History, a reload and a reconnect's catch-up never act.
  const readUpTo = useRef(Number.POSITIVE_INFINITY);
  const acted = useRef(new Set<number>());
  const devices = useMemo(() => deviceOps(shell.api), [shell]);
  // A port an agent offered from the PC the chat runs on, forwarded to this one (loopback only).
  const forwardHere = useCallback(
    (ref: ForwardRef) => {
      devices
        .forwardOpen({ chatId, port: ref.port, ...(ref.title ? { title: ref.title } : {}) })
        .then((f) =>
          toast.show({
            kind: 'info',
            text: forwardLine(f),
            action: { label: 'Stop', run: () => void devices.forwardClose(f.forwardId).catch(() => undefined) },
            ttlMs: 15_000,
          }),
        )
        .catch((e: unknown) => toast.show({ kind: 'error', text: `Port ${ref.port} was not forwarded: ${e instanceof Error ? e.message : String(e)}` }));
    },
    [devices, chatId],
  );
  const remoteOn = remote !== null;
  const isRemote = useRef(false);
  isRemote.current = remoteOn;
  // A tool server this chat was given that cannot connect on this PC gets one notice line:
  // read when the chat opens, and again whenever a status line moves. The host says why, in
  // words for the person. A chat that runs elsewhere uses the other PC's servers, which this
  // host cannot speak for — so no line for those.
  const mcp = useMemo(() => mcpOps(shell.api), [shell.api]);
  useEffect(() => {
    if (remote) {
      setMcpNotice(null);
      return;
    }
    let live = true;
    const ask = () =>
      void mcp
        .chatProblems(chatId)
        .then((p) => live && setMcpNotice(chatMcpNoticeText(p)), () => live && setMcpNotice(null));
    ask();
    const off = shell.bus.on('mcp_status', ask);
    return () => {
      live = false;
      off();
    };
  }, [mcp, shell.bus, chatId, remote]);
  useEffect(
    () =>
      shell.bus.on('message', (e) => {
        if (e.chatId !== chatId || !Array.isArray(e.rows)) return;
        const picks = autoOpenPicks(e.rows as MessageRow[], { after: readUpTo.current, done: acted.current, remote: isRemote.current });
        // A window nobody is looking at opens nothing; the rows are spent all the same.
        if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
        // A local chat's panel opens arrive as `panel_open` events, which reach their own chat's
        // panel whether or not it is in view. A chat running on another PC has no such event on
        // this PC: it opens from its rows while it is in view here.
        if (picks.panel && isRemote.current) void handleLink(picks.panel.ref.target, { chatId });
        if (picks.forward) forwardHere(picks.forward.ref);
      }),
    [shell, chatId, forwardHere],
  );
  // Pictures on disk named in replies and tool cards come from the host; for a chat that runs on
  // another PC, from that PC (the chat id tells this PC's host where to read).
  // Kept pictures are told apart by PC, since two PCs can have a picture at the same path.
  const pictureSource = remote?.uid ?? '';
  useEffect(
    () => setLocalImageLoader((path) => shell.api<{ mime: string; bytes: string }>('local_image_get', { path, chatId }), pictureSource),
    [shell, chatId, pictureSource],
  );
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
    // The chat opens as one window across its roll-over stack: an older session's rows keep
    // loading as the reader scrolls up, with a divider where the sessions meet.
    const [{ chat, thread }, page] = await Promise.all([ops.get(chatId), ops.page(chatId, { stack: true })]);
    store.chat(chat);
    store.thread(thread);
    store.page(page, 'replace');
    const top = page.messages.reduce((m, r) => Math.max(m, r.id), 0);
    readUpTo.current = Number.isFinite(readUpTo.current) ? Math.max(readUpTo.current, top) : top;
  }, [ops, store, chatId]);

  useEffect(() => {
    void reload().catch((e: unknown) => setNotice(`Could not load the chat: ${String((e as Error).message ?? e)}`));
  }, [reload, view.refreshes]);

  // "Around" jumps: load the page around a row, then scroll to it. Nothing
  // links to an old row yet; the dev console can call window.__helmJump(rowId).
  const jumpAround = useCallback(
    async (rowId: number) => {
      // The row may sit in any session of the stack; the host resolves it there.
      const page = await ops.page(chatId, { around: rowId, inclusive: true, stack: true });
      store.setKeep(chatId, 'oldest');
      store.page(page, 'replace');
      setJumpTo(rowId);
    },
    [ops, store, chatId],
  );
  // A selection that asked to land on one message (a chat link, an open event) jumps there once,
  // loading that row's page from wherever it sits in the stack. The request is taken once, whether
  // this chat was just opened or was already on screen.
  useEffect(() => {
    const take = () => {
      const f = shell.takeFocus(chatId);
      if (!f) return;
      const rowId = Number(f.messageId);
      if (Number.isFinite(rowId)) void jumpAround(rowId).catch(() => undefined);
    };
    take();
    return shell.subscribe(take);
  }, [shell, chatId, jumpAround]);
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
    if (!unreadToken || cleared.current !== null || remote) return;
    cleared.current = unreadToken;
    void ops.clearUnread(chatId, unreadToken).catch(() => undefined);
  }, [unreadToken, ops, chatId, remote]);
  const loaded = chat !== null;
  useEffect(() => {
    // Loaded without a mark: any mark made from now on is one the user made while here.
    if (loaded && unreadToken === null && cleared.current === null) cleared.current = '';
  }, [loaded, unreadToken]);
  const status = chat?.status;
  useEffect(() => {
    if (!completionId || !status || isBusy(status) || remote) return;
    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
    void ops.ackCompletion(chatId, completionId).catch(() => undefined);
  }, [completionId, status, ops, chatId, remote]);
  const behavior = thread?.sendBehavior ?? HOME_SEND_BEHAVIOR;
  const busy = chat ? isBusy(chat.status) : false;

  // The host's context reading for the context line: the chat's last measured call over the model's
  // real window. Read when the chat opens or its model changes, and once each turn settles — a
  // reading taken mid-turn is stale by half the turn, and the row's own usage covers the gap.
  const [ctxRead, setCtxRead] = useState<ChatContextReading | null>(null);
  useEffect(() => {
    if (busy || remote) return;
    let live = true;
    void shell
      .api<ChatContextReading>('chat_context', { chatId })
      .then((c) => live && setCtxRead(c))
      .catch(() => live && setCtxRead(null));
    return () => {
      live = false;
    };
  }, [chatId, busy, chat?.model, shell, remote]);

  const stopTool = useCallback(
    (toolCallId: string) => {
      void ops.stopTool(chatId, toolCallId).catch(() => undefined);
    },
    [ops, chatId],
  );
  const latestAssistantId = useMemo(() => [...view.rows].reverse().find((r) => r.role === 'assistant')?.id ?? null, [view.rows]);
  // Which note/page cards this chat records as opened: the state rides on the chat, so the
  // cards read open the same on every PC and survive the row virtualizing away. The op
  // answers with the fresh record, which lands in the store at once; the chat's refresh
  // event carries it to the other PCs.
  const openCards = useMemo(() => new Set(chat?.sidebarState?.openCards ?? []), [chat?.sidebarState]);
  // Card writes ride one after another: each builds its list from the record the previous
  // write answered with, not from the last record this view happened to hold — otherwise two
  // quick opens send two lists, each missing the other's card, and the later write erases the
  // first. The chain only delays the send by the round trip a single write already pays.
  const cardWrites = useRef(Promise.resolve());
  const setCardOpen = useCallback(
    (rowId: number, open: boolean) => {
      cardWrites.current = cardWrites.current
        .then(() => ops.setSidebarState(chatId, patchCardOpen(store.view(chatId).chat?.sidebarState, rowId, open)))
        .then((rec) => store.chat(rec))
        .catch(() => undefined);
    },
    [chatId, ops, store],
  );
  const interactionOpen = (thread?.interactions.length ?? 0) > 0;
  const mentionRepo = useMentionRepo(shell.api, remote ? undefined : chat?.cwd);
  // chatState rides along so the waiting lines can show the queue hold's own copy (and arm its
  // Start anyway button) instead of only what the live rows say.
  const queue = useMemo(
    () =>
      view.hasNewer
        ? undefined
        : remote
          ? { pending: view.pending, thread, ops, devOps, readOnly: !remote.state.live, chatState: chat?.state ?? null }
          : { pending: view.pending, thread, ops, readOnly: false, chatState: chat?.state ?? null },
    [view.hasNewer, view.pending, thread, ops, devOps, remote, chat],
  );
  const ctx: RowContext = useMemo(
    () => ({
      chatId,
      mentionRepo,
      canSignIn: !isLocalModelId(chat?.model),
      latestAssistantId,
      running: busy,
      interactionOpen,
      stopChecking: thread?.stopCheck != null,
      elements,
      editing,
      remoteDevice: remote?.state.deviceName,
      // Editing a sent message goes to the PC that runs the chat: the text reads from the
      // mirror's device and the edit applies there; a refusal (busy, archived) comes back as
      // that PC's own words.
      onEdit: (row: MessageRow) => {
        ops
          .editOpen(chatId, row.id)
          .then((o) => setEditing({ rowId: o.messageId, revision: o.revision, text: o.text, hasAttachments: o.hasAttachments === true }))
          .catch((e: unknown) => setNotice(e instanceof Error ? e.message : String(e)));
      },
      onResend: async (text: string) => {
        if (!editing) return;
        await ops.resend(chatId, editing.rowId, editing.revision, text);
        setEditing(null);
      },
      onCancelEdit: () => setEditing(null),
      onForward: remoteOn ? forwardHere : undefined,
      canStopTool: (row: MessageRow) =>
        behavior.toolStop && Boolean(thread?.capabilities.stopTool) && Boolean(row.details?.stoppable) && row.status === 'streaming',
      onStopTool: stopTool,
      // Every link click goes through the panel's gate, which validates it before acting and
      // shows any refusal in the shell's toast slot itself. File and page links resolve against
      // the session that wrote the row carrying the link — an earlier session of a roll-over
      // window may run elsewhere than the one shown in the window; without a session it is this chat.
      onLink: (t: LinkTarget, sessionId?: string) => void handleLink(t, { chatId: sessionId ?? chatId }),
      openCards,
      setCardOpen,
    }),
    [behavior.toolStop, thread?.capabilities.stopTool, thread?.stopCheck, stopTool, chatId, latestAssistantId, busy, interactionOpen, editing, ops, elements, mentionRepo, remote?.state.deviceName, chat?.model, remoteOn, forwardHere, openCards, setCardOpen],
  );

  // Escape while the chat works stops the turn; whatever it had not read goes back to it at
  // once. It is the lowest layer of the shell's Escape stack (menus, popovers, search, dialogs
  // and panels go first).
  useLayer(
    'composer',
    busy,
    () => {
      void ops.stop(chatId).catch(() => undefined);
    },
    () => busy,
  );

  const transcriptBox = useRef<HTMLDivElement>(null);
  // Ctrl+A then Ctrl+C copies the whole chat, not just the rows that are mounted.
  const wholeChat = useWholeChat(transcriptBox, (before) => ops.page(chatId, { ...(before ? { before } : {}), stack: true }), (text) => setNotice(text));
  // Find in chat: the host searches the whole history; a match in a row that is not
  // loaded loads the page around it first. A chat with a roll-over stack searches every session
  // of the stack in one answer instead of one chat's history.
  const hostFind = useCallback(
    (q: string, activity: boolean, after?: string) =>
      chat?.stackId
        ? ops.stackFind(chatId, q, activity).then((r) => ({ rows: stackFoundToHost(r.rows), nextCursor: null, truncated: r.truncated }))
        : ops.find(chatId, q, activity, after),
    [ops, chatId, chat?.stackId],
  );
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
  // Sending a message from this chat's composer scrolls to it (the composer decides, by its setting).
  const [scrollSignal, setScrollSignal] = useState(0);
  useEffect(() => onComposerSent(chatId, () => setScrollSignal((n) => n + 1)), [chatId]);
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
    if (view.chat?.sideSourceId || remote) return;
    return registerSelectionAction({
      id: 'side-chat',
      label: 'Side chat',
      appliesTo: (sel) => sel.chatId === chatId && sideChatAnchor(rowsRef.current, sel.rowIds) !== null,
      run: (sel) => {
        const anchor = sideChatAnchor(rowsRef.current, sel.rowIds);
        if (anchor === null) return;
        // The anchor reply may sit in an earlier session of the stack: that session holds it.
        const source = anchorSession(rowsRef.current, anchor, chatId);
        ops
          .fork(source, { messageId: anchor, side: true, excerpt: sel.text })
          .then((c) => {
            if (!openInPanel(SIDE_CHAT_KIND, chatId, { chatId: c.id, sourceId: source }, { intent: 'user', chatId })) {
              setNotice('The side panel has no room for the side chat.');
            }
          })
          .catch((e: unknown) => setNotice(`Could not open a side chat: ${e instanceof Error ? e.message : String(e)}`));
      },
    });
  }, [ops, chatId, view.chat?.sideSourceId, remote]);

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
      {/* A local chat's work reads as queue state, not just "running": what the header and the
          dock say, kept as text here for assistive tech and the e2e hook. */}
      {busy && isLocalModelId(chat.model) && (
        <span className="visually-hidden" data-testid="chat-local-status" data-ahead={chat.localAhead != null ? String(chat.localAhead) : undefined}>
          {chat.localAhead != null ? localWaitLabel(chat.localAhead) : LOCAL_WORKING_LABEL}
        </span>
      )}
      {remote && <RemoteChatBar state={remote.state} chatId={chatId} />}
      {mcpNotice && (
        <div className="notice" role="status" data-testid="chat-mcp-notice">
          {mcpNotice}
        </div>
      )}
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
        scrollSignal={scrollSignal}
        sessionMarks={view.sessionMarks}
        queue={queue}
        onLoadOlder={async () => {
          if (!view.olderCursor) return;
          store.page(await ops.page(chatId, { before: view.olderCursor, stack: true }), 'older');
        }}
        onJumpLatest={async () => {
          setJumpTo(null);
          store.setKeep(chatId, 'newest');
          if (view.hasNewer) store.page(await ops.page(chatId, { stack: true }), 'replace');
        }}
        onFollowChange={(f) => store.setKeep(chatId, f ? 'newest' : 'oldest')}
      />
      </div>
      {thread?.interactions.map((i) => (
        <InteractionCard key={i.id} interaction={i} lock={remote && !remote.state.live ? remote.state.text : null} onAnswer={(id, a) => void ops.answer(chatId, id, a).catch(() => undefined)} />
      ))}
      {/* The composer dock: banners, then other areas' dock items (the
          review tray), then the in-flight list, in one capped, scrolling live region above the box. */}
      <div className="composer-dock" role="region" aria-label="Composer" aria-live="polite" data-testid="composer-dock">
      <div className="composer-dock-stack">
      {chat.status === 'interrupted' && (
        <div className="banner" data-testid="interrupted-banner">
          {(() => {
            // An error written as a headline, a blank line and the raw text keeps the raw text
            // behind a fold (the plan-limit notice does this).
            const text = chat.error ?? 'This chat was interrupted. Nothing saved was lost; check the chat before continuing.';
            const cut = text.indexOf('\n\n');
            if (cut < 0) return <span>{text}</span>;
            return (
              <span>
                {text.slice(0, cut)}
                <details data-testid="interrupted-details">
                  <summary>Details</summary>
                  {text.slice(cut + 2)}
                </details>
              </span>
            );
          })()}
          {isTooLargeRefusal(chat.error) && (
            // The turn died on a prompt too large for the model's window. "Continue" would send
            // the same oversized prompt and fail again; the roll over is the way out.
            <button
              type="button"
              className="small"
              data-testid="interrupted-roll-over"
              onClick={() => setRollConfirm(rollOverConfirm(chat.title, () => rollOverChat(ops, chatId, (m) => setNotice(m))))}
            >
              Roll over
            </button>
          )}
          {isSignInError(chat.error) && <SignInAgain />}
          {isExecutableNotFound(chat.error) && (
            // The worker could not find Claude Code itself: the way out is the path in
            // Settings → Runtime, not another send.
            <button
              type="button"
              className="small"
              data-testid="interrupted-runtime-settings"
              onClick={() => {
                requestPane('runtime');
                shell.openDialog(SETTINGS_DIALOG);
              }}
            >
              Open Runtime settings
            </button>
          )}
          {lastUser && (
            <button
              type="button"
              className="small"
              data-testid="recover-draft"
              onClick={() => {
                setTextRef.current(lastUser.text);
                // The message's pictures and files are copied into the chat's unstaged set and
                // staged in the composer, so the recovered draft can carry them again.
                if (lastUser.details?.images?.length || lastUser.details?.refs?.length) {
                  void ops
                    .recoverAttachments(chat.id, lastUser.id)
                    .then(() => composerApi(chat.id).restore())
                    .catch(() => undefined);
                }
              }}
            >
              Recover draft
            </button>
          )}
        </div>
      )}
      {chat.archived && (
        <div className="banner" data-testid="archived-banner">
          <span>This chat is archived.</span>
          <button type="button" className="small" data-testid="archived-restore" onClick={() => ops.restore(chat.id).catch((e: unknown) => setNotice(String((e as Error).message ?? e)))}>
            Restore
          </button>
        </div>
      )}
      <ContextBar
        chat={chat}
        rows={view.rows}
        host={ctxRead}
        hasNewer={view.hasNewer}
        busy={busy}
        onRollOver={() => setRollConfirm(rollOverConfirm(chat.title, () => rollOverChat(ops, chatId, (m) => setNotice(m))))}
      />
      {/* Allow cards show on every device that has the chat open, the home PC included; on a
          mirrored chat the answer travels to the PC that runs it, and the first answer wins. */}
      {accessoriesAt('consent').map((a) => (
        <div key={a.id} className="composer-accessory" data-accessory={a.id}>
          {a.render({ chat, thread, busy, draftEmpty: !chat.draft, remote: !!remote })}
        </div>
      ))}
      {accessoriesAt('dock').map((a) => (
        <div key={a.id} className="composer-accessory" data-accessory={a.id}>
          {a.render({ chat, thread, busy, draftEmpty: !chat.draft })}
        </div>
      ))}
      {!chat.sideSourceId && <FlagNote chatId={chatId} ops={ops} devOps={remote ? devOps : undefined} />}
      {/* A transcript that is not at its newest page cannot end with them, so they stack here. */}
      {view.hasNewer && <QueueList pending={view.pending} thread={thread} ops={ops} chatId={chatId} readOnly={remote ? !remote.state.live : false} devOps={remote ? devOps : undefined} reason={holdLine(chat?.state, Date.now()) ?? waitReason(view.rows)} claudeHold={claudeHold(chat?.state)} rows={view.rows} />}
      </div>
      {/* The activity line sits directly on the box, below the in-flight messages. */}
      {thread && <ActivityDock chat={chat} thread={thread} turnStartedAt={turnStartedAt} onStopTool={stopTool} rows={view.hasNewer ? undefined : view.rows} runEndedAt={view.lastRunEndedAt} />}
      <Composer
        recall={recallPrompts(view.rows)}
        autocomplete={autocompleteOn}
        chat={chat}
        thread={thread}
        pending={view.pending}
        ops={ops}
        flushers={flushers}
        sendShortcut={sendShortcut}
        remote={remote?.composer}
        registerSetText={(f) => {
          setTextRef.current = f;
        }}
      />
      </div>
      {rollConfirm && <ConfirmDialog spec={rollConfirm} onClose={() => setRollConfirm(null)} />}
    </div>
  );
}
