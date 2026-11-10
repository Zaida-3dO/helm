// The sidebar's one-field prompt: Rename chat, Rename project, Project icon, Project colour and
// Project description all open it. It is one small component with one text field, so a feature that
// adds to the rename dialog (a hint under the field, suggested titles) extends this file.

import { useRef, useState } from 'react';
import { Button, Dialog, TextInput } from '../ui/index.ts';

export type Prompt = {
  title: string;
  label: string;
  value: string;
  /** The field's length limit (120 when absent). */
  maxLength?: number;
  /** Answers with why it failed (the dialog stays open and shows it), or nothing on success. */
  onSave: (v: string) => Promise<string | null | void> | void;
};

export function PromptDialog({ prompt, onClose }: { prompt: Prompt; onClose: () => void }) {
  const [value, setValue] = useState(prompt.value);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const field = useRef<HTMLInputElement>(null);
  const canSave = value !== prompt.value && !busy;
  const save = async () => {
    if (!canSave) return;
    setBusy(true);
    setError(null);
    let failed: string | null | void = null;
    try {
      failed = await prompt.onSave(value);
    } catch (e) {
      failed = String((e as Error)?.message ?? e);
    }
    setBusy(false);
    if (failed) {
      setError(failed);
      // The field was read-only while saving; hand focus back to it once it is editable again.
      setTimeout(() => field.current?.focus(), 0);
    } else onClose();
  };
  return (
    <Dialog
      title={prompt.title}
      onClose={onClose}
      busy={busy}
      padded
      testId="sidebar-prompt"
      footer={
        <>
          <Button variant="secondary" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void save()} disabled={value === prompt.value} busy={busy} data-testid="sidebar-prompt-save">
            {busy ? 'Saving…' : 'Save'}
          </Button>
        </>
      }
    >
      <label className="sb-prompt">
        <span>{prompt.label}</span>
        <TextInput
          ref={field}
          autoFocus
          value={value}
          maxLength={prompt.maxLength ?? 120}
          invalid={error}
          readOnly={busy}
          onChange={(e) => {
            setValue(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter') void save();
          }}
        />
      </label>
    </Dialog>
  );
}
