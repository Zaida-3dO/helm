// Settings → General: the settings a chat has changed (the host keeps the last 100) live behind
// one button — "Changes made by chats (N)" — that opens them in their own dialog, each row with
// Undo. Undo is refused by the host when the setting has changed since.

import { useCallback, useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Dialog, SettingRow, toast } from '../../ui/index.ts';
import { changedBy, changeText, keyTitle, parseChanges, type SettingChange } from './model.ts';

export function ChangedByChats() {
  const shell = useShell();
  const [changes, setChanges] = useState<SettingChange[]>([]);
  const [open, setOpen] = useState(false);
  const reload = useCallback(
    () =>
      shell
        .api<unknown>('settings_changes_list')
        .then((v) => setChanges(parseChanges(v)))
        .catch(() => undefined),
    [shell],
  );
  useEffect(() => {
    void reload();
    const offs = [shell.bus.on('external_settings', () => void reload()), shell.bus.on('preferences', () => void reload())];
    return () => offs.forEach((o) => o());
  }, [shell, reload]);
  // Nothing to look at until a chat has changed something; the button appears with the first change.
  if (changes.length === 0) return null;
  return (
    <div className="settings-action-row">
      <Button onClick={() => setOpen(true)} data-testid="changed-by-chats-open">
        Changes made by chats ({changes.length})
      </Button>
      {open && (
        <Dialog title="Changes made by chats" onClose={() => setOpen(false)} testId="changed-by-chats" padded>
          <ChangedList changes={changes} onReload={reload} />
        </Dialog>
      )}
    </div>
  );
}

function ChangedList({ changes, onReload }: { changes: SettingChange[]; onReload: () => void }) {
  const shell = useShell();
  const [busy, setBusy] = useState<number | null>(null);

  const undo = async (c: SettingChange) => {
    setBusy(c.changeId);
    try {
      await shell.api('settings_undo', { changeIds: [c.changeId] });
    } catch (e) {
      toast.show({ kind: 'error', text: String((e as Error).message ?? e) });
    } finally {
      setBusy(null);
      onReload();
    }
  };

  return (
    <div data-testid="changed-by-chats-list">
      {changes.map((c) => {
        const by = changedBy(c.chatId, shell.store.chatList());
        return (
          <SettingRow
            key={c.changeId}
            testId={`changed-${c.changeId}`}
            title={keyTitle(c.key)}
            description={`${changeText(c)} · by ${by.text} · ${new Date(c.at).toLocaleString()}`}
          >
            {by.known && (
              <Button variant="text" onClick={() => (shell.select(c.chatId), shell.closeDialog())} data-testid={`changed-open-${c.changeId}`}>
                Open chat
              </Button>
            )}
            {c.undone ? (
              <span className="muted">Undone</span>
            ) : (
              <Button disabled={busy !== null} onClick={() => void undo(c)}>
                Undo
              </Button>
            )}
          </SettingRow>
        );
      })}
    </div>
  );
}
