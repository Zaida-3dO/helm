// A small confirmation dialog: a question, a sentence on what happens, Cancel and the action.
// The action's button shows the wheel while it runs, and the dialog ignores Escape until it is done.
// A destructive confirmation starts with Cancel focused, so a stray Enter cannot delete anything.

import { useState, type ReactNode } from 'react';
import { Button } from './controls.tsx';
import { Dialog } from './Dialog.tsx';

export interface ConfirmSpec {
  title: string;
  body: ReactNode;
  /** The action button's label. */
  confirm: string;
  /** Destructive: the action is an outlined button in the error colour and Cancel has focus. */
  danger?: boolean;
  onConfirm: () => void | Promise<unknown>;
}

export function ConfirmDialog({ spec, onClose }: { spec: ConfirmSpec; onClose: () => void }) {
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      await spec.onConfirm();
    } finally {
      onClose();
    }
  };
  return (
    <Dialog
      title={spec.title}
      onClose={onClose}
      busy={busy}
      padded
      testId="confirm-dialog"
      footer={
        <>
          <Button variant="secondary" onClick={onClose} disabled={busy} data-autofocus={spec.danger ? '' : undefined}>
            Cancel
          </Button>
          <Button
            variant={spec.danger ? 'secondary' : 'primary'}
            danger={spec.danger}
            busy={busy}
            onClick={() => void run()}
            data-autofocus={spec.danger ? undefined : ''}
            data-testid="confirm-action"
          >
            {spec.confirm}
          </Button>
        </>
      }
    >
      <p className="ui-confirm-body">{spec.body}</p>
    </Dialog>
  );
}
