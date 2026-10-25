// Settings → General → Changed by chats: the settings a chat has changed (the host keeps the last
// 100), each with Undo. Undo is refused by the host when the setting has changed since.

import { useCallback, useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, SettingRow, toast } from '../../ui/index.ts';
import { changedBy, changeText, keyTitle, parseChanges, type SettingChange } from './model.ts';

const SHOWN = 8;

export function ChangedByChats() {
  const shell = useShell();
  const [changes, setChanges] = useState<SettingChange[]>([]);
  const [all, setAll] = useState(false);
  const [busy, setBusy] = useState<number | null>(null);
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

  const undo = async (c: SettingChange) => {
    setBusy(c.changeId);
    try {
      await shell.api('settings_undo', { changeIds: [c.changeId] });
    } catch (e) {
      toast.show({ kind: 'error', text: String((e as Error).message ?? e) });
    } finally {
      setBusy(null);
      void reload();
    }
  };

  const shown = all ? changes : changes.slice(0, SHOWN);
  return (
    <div data-testid="changed-by-chats">
      <h3 className="settings-section">Changed by chats</h3>
      {changes.length === 0 && <p>No chat has changed a setting yet.</p>}
      {shown.map((c) => {
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
      {changes.length > SHOWN && (
        <Button variant="text" onClick={() => setAll((v) => !v)}>
          {all ? 'Show fewer' : `Show all ${changes.length}`}
        </Button>
      )}
    </div>
  );
}
