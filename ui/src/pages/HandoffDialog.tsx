// The confirmation Helm shows when a page asks to send a message or open a link. It belongs to
// Helm, not to the page: the page cannot style, dismiss or answer it. Nothing leaves the page
// until the user picks an action here, and a page can never start work by itself.

import { useState } from 'react';
import { Button, Dialog, TextArea } from '../ui/index.ts';

export type HandoffChoice = 'send' | 'add';

export type HandoffRequest =
  | { kind: 'message'; title: string; text: string; chatName: string | null }
  | { kind: 'link'; url: string };

interface Props {
  request: HandoffRequest;
  /** Runs the chosen action; a rejection's message is shown and the dialog stays open. */
  onChoose: (choice: HandoffChoice | 'open', text: string) => Promise<void>;
  onCancel: () => void;
}

export function HandoffDialog({ request, onChoose, onCancel }: Props) {
  const [text, setText] = useState(request.kind === 'message' ? request.text : '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (choice: HandoffChoice | 'open') => {
    setBusy(true);
    setError(null);
    try {
      await onChoose(choice, text);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  if (request.kind === 'link') {
    return (
      <Dialog
        title="Open this link?"
        busy={busy}
        onClose={onCancel}
        padded
        testId="page-handoff"
        footer={
          <>
            <Button variant="secondary" onClick={onCancel} disabled={busy} data-testid="page-handoff-cancel">
              Cancel
            </Button>
            <Button variant="primary" busy={busy} onClick={() => void run('open')} data-testid="page-handoff-open">
              Open link
            </Button>
          </>
        }
      >
        <p className="pg-handoff-note">A page you are running asked to open this address in your browser.</p>
        <p className="pg-handoff-url" data-testid="page-handoff-url">
          {request.url}
        </p>
        {error && <p className="pg-handoff-error">{error}</p>}
      </Dialog>
    );
  }

  const chat = request.chatName;
  return (
    <Dialog
      title={request.title}
      busy={busy}
      onClose={onCancel}
      padded
      testId="page-handoff"
      footer={
        <>
          <Button variant="secondary" onClick={onCancel} disabled={busy} data-testid="page-handoff-cancel">
            Cancel
          </Button>
          <Button variant="secondary" disabled={busy || !text.trim() || !chat} onClick={() => void run('add')} data-testid="page-handoff-add">
            Add to message
          </Button>
          <Button variant="primary" busy={busy} disabled={!text.trim() || !chat} onClick={() => void run('send')} data-testid="page-handoff-send">
            Send to agent
          </Button>
        </>
      }
    >
      <p className="pg-handoff-note">
        {chat ? `A page you are running wants to message “${chat}”. Edit it if you like; nothing is sent until you choose.` : 'This page is not linked to a chat, so there is nowhere to send this. Link it to a chat from the page’s toolbar.'}
      </p>
      <TextArea
        className="pg-handoff-text"
        aria-label="Message from page"
        value={text}
        rows={8}
        maxLength={16_000}
        onChange={(e) => setText(e.target.value)}
        disabled={busy}
        data-testid="page-handoff-text"
      />
      {error && <p className="pg-handoff-error">{error}</p>}
    </Dialog>
  );
}
