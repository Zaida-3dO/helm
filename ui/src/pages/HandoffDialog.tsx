// The confirmation Helm shows when a page asks to open a link. A page's messages go straight to
// the chat (the card there says the page wrote them), so this dialog is only for links: they
// leave the machine for the open browser, so Helm still asks. It belongs to Helm, not to the
// page: the page cannot style, dismiss or answer it.

import { useState } from 'react';
import { Button, Dialog } from '../ui/index.ts';

export type HandoffRequest = { kind: 'link'; url: string };

interface Props {
  request: HandoffRequest;
  /** Runs the chosen action; a rejection's message is shown and the dialog stays open. */
  onChoose: () => Promise<void>;
  onCancel: () => void;
}

export function HandoffDialog({ request, onChoose, onCancel }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      await onChoose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

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
          <Button variant="primary" busy={busy} onClick={() => void run()} data-testid="page-handoff-open">
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
