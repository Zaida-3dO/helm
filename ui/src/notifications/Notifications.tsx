// The in-app completion stack: bottom-right,
// one notice per chat with Open, Continue and Reply. Continue and Reply are the user's own sends
// (origin "notification": they follow send behaviour and never touch the draft). In Windows
// mode the host posts native toasts instead and this stack stays empty.

import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { shellOps } from '../api/shell.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { Button, Icon, TextInput, toast } from '../ui/index.ts';
import { NoticeStack, type CompletionLike, type Notice } from './model.ts';
import './notifications.css';

const REPLY_MAX = 16 * 1024;

export function Notifications() {
  const shell = useShell();
  const { chatId } = useSelection();
  const stack = useMemo(() => new NoticeStack(), []);
  const notices = useSyncExternalStore(stack.subscribe, stack.list);
  const [mode, setMode] = useState<'in_app' | 'native'>('in_app');

  useEffect(() => {
    const load = () =>
      shellOps(shell.api)
        .get()
        .then((v) => setMode(v.settings.notifications))
        .catch(() => undefined);
    void load();
    return shell.bus.on('preferences', (e) => {
      if ('notifications' in ((e as { changed?: object }).changed ?? {})) void load();
    });
  }, [shell]);

  useEffect(() => {
    const offC = shell.bus.on('completion', (e) => {
      if (mode !== 'in_app') return;
      const c = (e as { completion?: CompletionLike }).completion;
      if (c) stack.completion(c, document.hasFocus() ? shell.selection.chatId : null);
    });
    const offA = shell.bus.on('attention', (e) => {
      const s = (e as { summary?: { unread?: { chatId: string }[] } }).summary;
      if (s?.unread) stack.summary(s.unread.map((r) => r.chatId));
    });
    return () => {
      offC();
      offA();
    };
  }, [shell, stack, mode]);

  // Opening the chat reads it: its notice goes.
  useEffect(() => {
    if (chatId) stack.withdraw(chatId);
  }, [chatId, stack]);
  useEffect(() => {
    if (mode !== 'in_app') stack.clear();
  }, [mode, stack]);

  if (notices.length === 0) return null;
  return (
    <div className="notice-stack" aria-live="polite">
      {notices.map((n) => (
        <CompletionNotice key={n.chatId} n={n} onClose={() => stack.withdraw(n.chatId)} />
      ))}
    </div>
  );
}

function CompletionNotice({ n, onClose }: { n: Notice; onClose: () => void }) {
  const shell = useShell();
  const [replying, setReplying] = useState(false);
  const [text, setText] = useState('');
  const send = (t: string) => {
    const body = t.trim();
    if (!body) return;
    if (new TextEncoder().encode(body).length > REPLY_MAX) {
      toast.show({ kind: 'error', text: 'Replies are limited to 16 KiB.' });
      return;
    }
    shell
      .api('chat_send', { chatId: n.chatId, submissionId: globalThis.crypto.randomUUID(), text: body, action: 'default', draftRevision: 0, origin: 'notification' })
      .then(onClose)
      .catch((e: unknown) => toast.show({ kind: 'error', text: `Couldn't send to "${n.title}": ${String((e as Error).message ?? e)}` }));
  };
  return (
    <div className="ui-toast completion-notice" role="status" data-testid="toast" data-chat-id={n.chatId}>
      <div className="notice-main">
        <div className="notice-head">
          <span className="notice-title">{n.title}</span>
          {n.project && <span className="notice-project">{n.project}</span>}
          <button type="button" className="ui-btn ui-btn-icon notice-x" aria-label="Dismiss notice" title="Dismiss" onClick={onClose}>
            <Icon name="x" />
          </button>
        </div>
        {n.preview && <div className="notice-preview">{n.preview}</div>}
        {replying ? (
          <div className="notice-reply">
            <TextInput
              autoFocus
              placeholder="Reply…"
              aria-label="Reply"
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') send(text);
                if (e.key === 'Escape') {
                  e.stopPropagation();
                  setReplying(false);
                }
              }}
            />
            <Button variant="primary" onClick={() => send(text)}>
              Send
            </Button>
          </div>
        ) : (
          <div className="notice-actions">
            <Button
              variant="text"
              data-action="open"
              onClick={() => {
                shell.select(n.chatId);
                onClose();
              }}
            >
              Open
            </Button>
            <Button variant="text" data-action="continue" onClick={() => send('Continue')}>
              Continue
            </Button>
            <Button variant="text" data-action="reply" onClick={() => setReplying(true)}>
              Reply
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
