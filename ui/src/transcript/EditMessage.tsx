// Inline edit of a sent message: change the text, then Resend (Mod+Enter) or
// Cancel (Escape). Resending replaces every later message; it does not undo changes to project
// files, leaves the draft alone, and keeps the earlier conversation in the local history.

import { useEffect, useRef, useState } from 'react';

export interface EditState {
  rowId: number;
  revision: number;
  text: string;
}

export const EDIT_NOTE =
  'Resending replaces every later message. Changes the agent made to files are not undone, your draft stays as it is, and the earlier conversation is kept in the history.';

export function EditMessage({
  edit,
  onResend,
  onCancel,
}: {
  edit: EditState;
  onResend: (text: string) => Promise<void>;
  onCancel: () => void;
}) {
  const [text, setText] = useState(edit.text);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => ref.current?.focus(), []);

  const submit = async () => {
    if (!text.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onResend(text);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <div className="edit-message" data-testid="edit-message">
      <textarea
        ref={ref}
        value={text}
        rows={Math.min(12, Math.max(3, text.split('\n').length))}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            void submit();
          } else if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            onCancel();
          }
        }}
      />
      <div className="edit-note muted">{EDIT_NOTE}</div>
      {error && <div className="error">{error}</div>}
      <div className="edit-actions">
        <button type="button" className="small" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
        <button type="button" className="small primary" data-testid="edit-resend" onClick={() => void submit()} disabled={busy || !text.trim()}>
          Resend
        </button>
      </div>
    </div>
  );
}
