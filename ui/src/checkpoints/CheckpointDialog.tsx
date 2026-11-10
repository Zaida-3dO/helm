// The confirmation for going back to before a message: says what comes back (the conversation) and
// what does not (files), lists the files the agent changed since, and makes the copy.

import { useEffect, useRef, useState } from 'react';
import type { Api } from '../api/client.ts';
import type { ChatRecord, MessageRow } from '../api/types.ts';
import { Button, Dialog } from '../ui/index.ts';
import { checkpointNotes, filesHeading, imageCount, moreFilesLine, type CheckpointInfo } from './model.ts';

export interface CheckpointDialogProps {
  api: Api;
  chatId: string;
  row: MessageRow;
  /** Open the copy. */
  onOpen: (chatId: string) => void;
  onClose: () => void;
}

export function CheckpointDialog({ api, chatId, row, onOpen, onClose }: CheckpointDialogProps) {
  const [info, setInfo] = useState<CheckpointInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // One fork id per dialog, so a repeated click answers the same copy.
  const forkId = useRef(globalThis.crypto.randomUUID());

  useEffect(() => {
    let live = true;
    api<CheckpointInfo>('checkpoint_info', { chatId, messageId: row.id })
      .then((i) => live && setInfo(i))
      .catch((e: unknown) => live && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [api, chatId, row.id]);

  const goBack = () => {
    setBusy(true);
    setError(null);
    api<ChatRecord>('chat_fork', { chatId, forkId: forkId.current, messageId: row.id })
      .then((c) => {
        onClose();
        onOpen(c.id);
      })
      .catch((e: unknown) => {
        setBusy(false);
        setError(e instanceof Error ? e.message : String(e));
      });
  };

  return (
    <Dialog
      title="Go back to before this message?"
      onClose={onClose}
      busy={busy}
      padded
      testId="checkpoint-dialog"
      footer={
        <>
          <Button variant="secondary" onClick={onClose} disabled={busy} data-autofocus="">
            Cancel
          </Button>
          <Button variant="primary" busy={busy} disabled={!info} onClick={goBack} data-testid="checkpoint-go-back">
            Go back
          </Button>
        </>
      }
    >
      {info && (
        <div data-testid="checkpoint-notes">
          {checkpointNotes(info, imageCount(row)).map((n) => (
            <p key={n} className="ui-confirm-body">
              {n}
            </p>
          ))}
          <p className="ui-confirm-body">{filesHeading(info)}</p>
          {info.changedFiles.length > 0 && (
            <ul data-testid="checkpoint-files">
              {info.changedFiles.map((f) => (
                <li key={f}>
                  <code>{f}</code>
                </li>
              ))}
              {info.moreFiles > 0 && <li>{moreFilesLine(info)}</li>}
            </ul>
          )}
        </div>
      )}
      {!info && !error && <p className="ui-confirm-body">Looking at what has happened since…</p>}
      {error && (
        <p className="ui-confirm-body" role="alert">
          {error}
        </p>
      )}
    </Dialog>
  );
}
