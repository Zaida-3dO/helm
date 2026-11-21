// One running page: the sealed frame, its toolbar and the bridge that lets it ask Helm for the
// three things a page may ask for (save state, send a message, open a link).
//
// The frame is `<iframe sandbox="allow-scripts">` with no `allow-same-origin`, so
// the page has an opaque origin and no way to reach this page, Helm's IPC, cookies or storage. It
// loads from the `helmpage` scheme under a random per-open address. The network follows the pin:
// a page pinned for the acting profile runs online and everything else stays offline; the host
// decides that when it opens the session, not this frame. Pinning or unpinning here ends the
// session and starts another so the rule takes effect at once. Where the web view cannot keep an
// offline page's scripts off the network (the host tells, per open), an offline page is shown
// without scripts and says so, with pinning the page as the way to run them.
//
// The bar keeps one button, Reload, which is what a page needs day to day. The rest are rare and
// sit in its "…" menu: adding to or removing from the Pages list, sending the page's messages to
// the chat in view, showing the file in File Explorer, and resetting its storage.
//
// The frame is read and polled only while the tab is shown. A rewrite of the file by an agent
// shows up as a new revision at the next poll and the frame reloads in place, with its saved
// state handed back to it.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { ApiError } from '../api/client.ts';
import type { Api } from '../api/client.ts';
import type { ChatRecord } from '../api/types.ts';
import { composerApi } from '../composer/slots.ts';
import { sendActionText } from '../actions/send.ts';
import { Button, ConfirmDialog, ContextMenu, toast } from '../ui/index.ts';
import type { ConfirmSpec, MenuItem } from '../ui/index.ts';
import { IconMore, IconRefresh, IconX } from '../ui/icons.tsx';
import { pageOps } from './api.ts';
import type { PageSession, PinnedPage } from './api.ts';
import type { PageRemote } from './open.ts';
import { PageBridge } from './bridge.ts';
import { HandoffDialog } from './HandoffDialog.tsx';
import type { HandoffChoice, HandoffRequest } from './HandoffDialog.tsx';
import { checkHandoff, checkLink, checkState, composeHandoff, handoffTitle, pageSandbox } from './protocol.ts';
import { pageTheme } from './theme.ts';
import './pages.css';

/** How often an open, shown page looks at its file. */
export const WATCH_MS = 1500;
const BUSY = 'This app cannot request that action right now.';

interface Pending {
  request: HandoffRequest;
  run: (choice: HandoffChoice | 'open', text: string) => Promise<void>;
  cancel: () => void;
}

export interface PageFrameProps {
  api: Api;
  /** The page file, absolute. */
  path: string;
  /** Set when the page is pinned on another PC: it is brought across to run here, and its messages go to the chat the pin names. */
  remote?: PageRemote;
  /** The chat in view when the page was opened: where messages go if the page is not linked. */
  chatId: string | null;
  /** The tab is shown. A page is not read before this is first true. */
  visible: boolean;
  onTitle?: (title: string) => void;
}

function words(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function PageFrame({ api, path, remote, chatId, visible, onTitle }: PageFrameProps) {
  const ops = useMemo(() => pageOps(api), [api]);
  const [shown, setShown] = useState(visible);
  const [nonce, setNonce] = useState(0);
  const [session, setSession] = useState<PageSession | null>(null);
  const [frameRev, setFrameRev] = useState(0);
  const [pin, setPin] = useState<PinnedPage | null>(null);
  const [openError, setOpenError] = useState<string | null>(null);
  const [runtimeError, setRuntimeError] = useState<string | null>(null);
  const [blocked, setBlocked] = useState(false);
  const [gone, setGone] = useState(false);
  const [unreadable, setUnreadable] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [linkedName, setLinkedName] = useState<string | null>(null);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);
  const moreRef = useRef<HTMLButtonElement>(null);

  const frameEl = useRef<HTMLIFrameElement>(null);
  const bridge = useRef<PageBridge | null>(null);
  const visibleRef = useRef(visible);
  const pendingRef = useRef<Pending | null>(null);
  const revisionRef = useRef(0);
  const stored = useRef<{ state: unknown; revision: number }>({ state: null, revision: 0 });
  const pinRef = useRef<PinnedPage | null>(null);
  const chatRef = useRef(chatId);
  const sessionRef = useRef<PageSession | null>(null);
  visibleRef.current = visible;
  pinRef.current = pin;
  chatRef.current = chatId;
  sessionRef.current = session;

  useEffect(() => {
    if (visible) setShown(true);
  }, [visible]);

  // Open a session once the tab is first shown, and again when Reload or a pin change ends one.
  useEffect(() => {
    if (!shown) return;
    let cancelled = false;
    let opened: PageSession | null = null;
    setOpenError(null);
    setSession(null);
    (remote ? ops.openRemote(remote.deviceUid, remote.pageId) : ops.open(path, chatRef.current))
      .then((s) => {
        if (cancelled) {
          void ops.close(s.session).catch(() => undefined);
          return;
        }
        opened = s;
        stored.current = { state: s.state ?? null, revision: s.stateRevision };
        revisionRef.current = s.revision;
        setPin(s.pins.find((p) => p.chatId) ?? s.pins[0] ?? null);
        setBlocked(false);
        setRuntimeError(null);
        setGone(false);
        setUnreadable(null);
        setSession(s);
        onTitle?.(s.pins[0]?.title ?? s.title);
      })
      .catch((e: unknown) => !cancelled && setOpenError(words(e)));
    return () => {
      cancelled = true;
      if (opened) void ops.close(opened.session).catch(() => undefined);
    };
    // onTitle is the tab's setter and changes every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ops, path, remote?.deviceUid, remote?.pageId, nonce, shown]);

  // The file watch.
  useEffect(() => {
    if (!session || !visible) return;
    let cancelled = false;
    const look = async () => {
      try {
        const s = await ops.status(session.session);
        if (cancelled) return;
        setGone(s.missing);
        setUnreadable(s.error);
        if (s.revision !== revisionRef.current) {
          revisionRef.current = s.revision;
          setFrameRev((r) => r + 1);
        }
      } catch (e) {
        // The host forgot the session (an idle one is dropped): start another.
        if (!cancelled && e instanceof ApiError && e.code === 'not_found') setNonce((n) => n + 1);
      }
    };
    const timer = setInterval(() => void look(), WATCH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [ops, session, visible]);

  // Which chat the page's messages go to: the pin's, else the chat the tab was opened beside.
  const targetChat = pin?.chatId ?? chatId;
  useEffect(() => {
    setLinkedName(null);
    if (!targetChat) return;
    let cancelled = false;
    api<{ chat: ChatRecord }>('chat_get', { chatId: targetChat })
      .then((r) => !cancelled && setLinkedName(r.chat.archived ? null : r.chat.title || 'this chat'))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [api, targetChat]);
  const nameRef = useRef<string | null>(null);
  nameRef.current = linkedName;

  const ask = useCallback(
    (request: HandoffRequest, perform: (choice: HandoffChoice | 'open', text: string) => Promise<unknown>) =>
      new Promise<unknown>((resolve, reject) => {
        if (pendingRef.current) return reject(new Error(BUSY));
        const done = () => {
          pendingRef.current = null;
          setPending(null);
        };
        const p: Pending = {
          request,
          run: async (choice, text) => {
            const r = await perform(choice, text);
            done();
            resolve(r);
          },
          cancel: () => {
            done();
            reject(new Error('Cancelled.'));
          },
        };
        pendingRef.current = p;
        setPending(p);
      }),
    [],
  );

  const handle = useCallback(
    async (method: string, payload: unknown): Promise<unknown> => {
      const s = sessionRef.current;
      if (!s) throw new Error(BUSY);
      if (method === 'setState') {
        const c = checkState(payload);
        if (!c.ok) throw new Error(c.error);
        const r = await ops.saveState(s.session, c.value.state, c.value.revision);
        stored.current = { state: c.value.state, revision: r.revision };
        return { revision: r.revision };
      }
      if (method === 'sendMessage') {
        const c = checkHandoff(payload);
        if (!c.ok) throw new Error(c.error);
        const text = composeHandoff(c.value, s.title);
        const target = pinRef.current?.chatId ?? chatRef.current;
        if (!target || !nameRef.current) throw new Error(BUSY);
        return ask(
          { kind: 'message', title: handoffTitle(c.value, s.title), text, chatName: nameRef.current },
          async (choice, edited) => {
            if (choice === 'add') {
              if (!composerApi(target).insert(edited)) throw new Error('Open the chat first, then add the message to it.');
              return { action: 'add' };
            }
            await sendActionText(api, target, edited);
            return { action: 'send' };
          },
        );
      }
      if (method === 'openLink') {
        const c = checkLink(payload);
        if (!c.ok) throw new Error(c.error);
        return ask({ kind: 'link', url: c.value }, async () => {
          await api('open_external', { url: c.value });
          return { action: 'open' };
        });
      }
      throw new Error('Unsupported app request.');
    },
    [api, ops, ask],
  );
  const handleRef = useRef(handle);
  handleRef.current = handle;

  // The bridge for this session; it hears only this session's frame. It listens before the frame can
  // load, so the page's first message is never missed.
  useLayoutEffect(() => {
    if (!session) return;
    const b = new PageBridge({
      channel: session.channel,
      frame: () => frameEl.current?.contentWindow ?? null,
      // The page's origin is opaque, so no target origin would match; the message is for that frame only.
      post: (m) => frameEl.current?.contentWindow?.postMessage(m, '*'),
      visible: () => visibleRef.current,
      request: (method, payload) => handleRef.current(method, payload),
      onReady: () =>
        b.send('init', {
          state: stored.current.state,
          revision: stored.current.revision,
          theme: pageTheme(),
          visible: visibleRef.current,
        }),
      onError: setRuntimeError,
      onNetworkBlocked: () => setBlocked(true),
    });
    bridge.current = b;
    const listener = (e: MessageEvent) => b.receive({ source: e.source, data: e.data });
    window.addEventListener('message', listener);
    const mo = new MutationObserver(() => b.send('theme', { theme: pageTheme() }));
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-skin'] });
    return () => {
      mo.disconnect();
      window.removeEventListener('message', listener);
      b.close();
      if (bridge.current === b) bridge.current = null;
    };
  }, [session]);

  useEffect(() => {
    bridge.current?.send('visibility', { visible });
  }, [visible]);

  // A confirmation left open is cancelled when the page goes away.
  useEffect(
    () => () => {
      pendingRef.current?.cancel();
    },
    [],
  );
  useEffect(() => {
    if (!session) pendingRef.current?.cancel();
  }, [session]);

  const reload = () => {
    if (!session) return;
    // A page from another PC is brought across again: its file there is not watched from here.
    if (remote) {
      setNonce((n) => n + 1);
      return;
    }
    ops
      .reload(session.session)
      .then((r) => {
        revisionRef.current = r.revision;
        setFrameRev((n) => n + 1);
      })
      .catch((e: unknown) => toast.show({ kind: 'error', text: words(e) }));
  };

  const togglePin = () => {
    if (!session) return;
    const action = pin
      ? ops.unpin(pin.id).then(() => setPin(null))
      : ops.pin(session.path, { chatId }).then((r) => setPin(r.page));
    // The network follows the pin, and the host fixes it when the session opens: end this open
    // so the new rule takes effect at once.
    action
      .then(() => setNonce((n) => n + 1))
      .catch((e: unknown) => toast.show({ kind: 'error', text: words(e) }));
  };

  // Show the page's file in File Explorer; the panel's own reveal op, same gate as the page.
  const revealHere = () => {
    if (!session) return;
    api('file_reveal', { path: session.path }).catch((e: unknown) =>
      toast.show({ kind: 'error', text: words(e) }),
    );
  };

  const linkHere = () => {
    if (!pin || !chatId) return;
    ops
      .linkChat(pin.id, chatId)
      .then(() => setPin({ ...pin, chatId }))
      .catch((e: unknown) => toast.show({ kind: 'error', text: words(e) }));
  };

  const resetStorage = () =>
    setConfirm({
      title: 'Reset this page’s storage?',
      body: 'The page forgets everything it saved and starts again from its first screen. The page file itself is not touched.',
      confirm: 'Reset storage',
      danger: true,
      onConfirm: async () => {
        if (!session) return;
        try {
          await ops.resetState(session.session);
          stored.current = { state: null, revision: 0 };
          setFrameRev((n) => n + 1);
        } catch (e) {
          toast.show({ kind: 'error', text: words(e) });
        }
      },
    });

  if (openError) {
    return (
      <div className="pg-root" data-testid="page-frame-root">
        <div className="pg-message" role="alert" data-testid="page-error">
          {openError}
          <button type="button" className="pg-link" onClick={() => setNonce((n) => n + 1)}>
            Try again
          </button>
        </div>
      </div>
    );
  }
  if (!session) {
    return (
      <div className="pg-root" data-testid="page-frame-root">
        <div className="pg-message">{shown ? 'Opening the page…' : ''}</div>
      </div>
    );
  }

  const canLinkHere = Boolean(!remote && pin && chatId && pin.chatId !== chatId);
  const menuItems: MenuItem[] = [
    // Pinning belongs to the PC that holds the page.
    ...(remote ? [] : [{ id: 'pin', label: pin ? 'Remove from Pages' : 'Add to Pages', onSelect: togglePin }]),
    ...(canLinkHere ? [{ id: 'link-chat', label: 'Send its messages to this chat', onSelect: linkHere }] : []),
    ...(remote ? [] : [{ id: 'reveal', label: 'Show in File Explorer', onSelect: revealHere }]),
    { id: 'reset', label: 'Reset page storage…', danger: true, separator: true, onSelect: resetStorage },
  ];
  return (
    <div className="pg-root" data-testid="page-frame-root" data-network={session.network ? 'on' : 'off'} data-scripts={session.scripts ? 'on' : 'off'}>
      <div className="pg-toolbar" role="toolbar" aria-label="Page controls">
        <span className="pg-name" title={session.path}>
          {session.title}
        </span>
        <Button variant="icon" label="Reload page" title={remote ? 'Bring the page across again' : 'Reload the page file'} onClick={reload} data-testid="page-reload">
          <IconRefresh size={14} />
        </Button>
        <Button
          ref={moreRef}
          variant="icon"
          label="More page actions"
          aria-haspopup="menu"
          aria-expanded={menuAt !== null}
          onClick={() => {
            const r = moreRef.current?.getBoundingClientRect();
            setMenuAt(menuAt ? null : { x: r ? r.right - 230 : 0, y: r ? r.bottom + 4 : 0 });
          }}
          data-testid="page-more"
        >
          <IconMore size={14} />
        </Button>
      </div>
      {menuAt && <ContextMenu x={menuAt.x} y={menuAt.y} label="Page actions" testId="page-menu" items={menuItems} onClose={() => setMenuAt(null)} />}
      {!session.scripts && (
        <div className="pg-notice" data-testid="page-scripts-off">
          Scripts are off while this page is offline: this version of the web view cannot keep them
          from reaching the network. Add it to Pages to let it use the network and run its scripts.
        </div>
      )}
      {blocked && (
        <div className="pg-notice" data-testid="page-network-hint">
          The page tried to use the network, which offline pages are kept from. Add it to Pages to
          let it.
        </div>
      )}
      {gone && <div className="pg-notice">The page file is gone. This is the last copy that was read.</div>}
      {unreadable && !gone && <div className="pg-notice">The page file changed but cannot be read yet: {unreadable}</div>}
      {runtimeError && (
        <div className="pg-error" role="alert" data-testid="page-runtime-error">
          <span>{runtimeError}</span>
          <Button variant="icon" label="Dismiss page error" onClick={() => setRuntimeError(null)}>
            <IconX size={12} />
          </Button>
        </div>
      )}
      <iframe
        key={`${session.session}:${frameRev}`}
        ref={frameEl}
        className="pg-frame"
        title={session.title}
        src={`${session.url}?r=${frameRev}`}
        sandbox={pageSandbox(session.scripts)}
        referrerPolicy="no-referrer"
        allow=""
        data-testid="page-frame"
      />
      {pending && <HandoffDialog request={pending.request} onChoose={pending.run} onCancel={pending.cancel} />}
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}
