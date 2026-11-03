// The read-only view of one of Claude Code's own chats (JSONL records Claude Code wrote on this
// PC). Nothing here edits those records. "Continue in Helm" copies the visible history into a new
// Helm chat that carries on from Claude Code's session; that is the only change this makes.

import { useEffect, useState } from 'react';
import { claudeChatOps, type ChatRead } from '../api/claudechats.ts';
import { useDialog, useShell } from '../shell/context.ts';
import { Button, Dialog } from '../ui/index.ts';
import { CLAUDE_CHAT_DIALOG, setClaudeChatSession, usePendingClaudeChatSession } from './index.ts';
import './claudechats.css';

export function ClaudeChatViewer() {
  const shell = useShell();
  const { open, close } = useDialog(CLAUDE_CHAT_DIALOG);
  const sessionId = usePendingClaudeChatSession();
  const [read, setRead] = useState<ChatRead | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open || !sessionId) return;
    let live = true;
    setRead(null);
    setNote(null);
    claudeChatOps(shell.api)
      .read(sessionId)
      .then((r) => live && setRead(r))
      .catch((e) => live && setNote(messageOf(e)));
    return () => {
      live = false;
    };
  }, [open, sessionId, shell]);

  if (!open) return null;
  const sessionIdForThisView = sessionId;

  const closeForGood = () => {
    setClaudeChatSession(null);
    close();
  };

  const continueInHelm = () => {
    if (!sessionIdForThisView || busy) return;
    setBusy(true);
    setNote(null);
    claudeChatOps(shell.api)
      .fork(sessionIdForThisView)
      .then((f) => {
        setBusy(false);
        closeForGood();
        shell.select(f.chatId);
      })
      .catch((e) => {
        setBusy(false);
        setNote(messageOf(e));
      });
  };

  const forkable = read?.forkable === true;
  return (
    <Dialog
      title={read?.title || 'Claude Code chat'}
      wide
      busy={busy}
      onClose={closeForGood}
      testId="claude-chat-viewer"
      footer={
        <div className="ccv-footer">
          <span className="ccv-note muted" data-testid="claude-chat-note">
            {note ?? (forkable || !read ? 'The record stays as it is; continuing only copies it.' : 'Its folder is gone, so it cannot be continued here.')}
          </span>
          {read && (
            <Button variant="primary" busy={busy} disabled={!forkable} onClick={continueInHelm} data-testid="claude-chat-continue">
              Continue in Helm
            </Button>
          )}
        </div>
      }
    >
      {read?.truncated && <p className="ccv-cut muted">This shows the newest part; the older lines did not fit.</p>}
      {read?.folder && (
        <p className="ccv-folder muted" data-testid="claude-chat-folder">
          {read.folder}
        </p>
      )}
      {note && !read && <p className="ccv-error error">{note}</p>}
      {read && (
        <div className="ccv-lines" data-testid="claude-chat-lines">
          {read.lines.map((l, i) => (
            <div key={i} className="ccv-line" data-testid="claude-chat-line" data-kind={l.kind}>
              <div className="ccv-line-label">
                {l.kind === 'tool' ? l.toolName || 'tool' : l.kind === 'context' ? '' : l.role === 'user' ? 'You' : 'Claude'}
              </div>
              <pre className="ccv-line-text">{l.text}</pre>
            </div>
          ))}
        </div>
      )}
    </Dialog>
  );
}

function messageOf(e: unknown): string {
  const m = (e as { message?: unknown })?.message;
  return typeof m === 'string' && m ? m : 'The record could not be read.';
}
