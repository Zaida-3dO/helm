// The confirmation for archiving a chat whose task is still running: the task stops first.

import { Button, Dialog } from '../ui/index.ts';

export function StopArchiveDialog({ title, onConfirm, onClose }: { title: string; onConfirm: () => void; onClose: () => void }) {
  return (
    <Dialog
      title="Stop and archive this chat?"
      onClose={onClose}
      testId="stop-archive-dialog"
      intro={`"${title.length > 80 ? `${title.slice(0, 79)}…` : title}" is still working. It stops now, then the chat is archived.`}
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            data-testid="stop-archive-confirm"
            onClick={() => {
              onClose();
              onConfirm();
            }}
          >
            Stop and archive
          </Button>
        </>
      }
    >
      {null}
    </Dialog>
  );
}
