// Settings → Data: where Helm keeps its data, a backup of it, and restoring one. A backup is a
// plain folder under Documents (a consistent copy of the database plus the chat logs, artifacts and
// attachments; no sign-ins or keys ever go in). A restore replaces the chats, the settings and the
// approvals, so it asks first, says what it replaces, and only takes effect at the next start.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { backupOps, RESTORE_CONFIRM, type BackupEntry, type BackupList } from '../../api/backup.ts';
import { useShell } from '../../shell/context.ts';
import { Button, ConfirmDialog, SettingRow, type ConfirmSpec } from '../../ui/index.ts';
import './data.css';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

function size(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

export function DataPane() {
  const shell = useShell();
  const ops = useMemo(() => backupOps(shell.api), [shell.api]);
  const [list, setList] = useState<BackupList | null>(null);
  const [profileDir, setProfileDir] = useState('');
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);

  const load = useCallback(
    () =>
      ops
        .list()
        .then(setList)
        .catch((e: unknown) => setError(errorText(e))),
    [ops],
  );

  useEffect(() => {
    void load();
    shell
      .api<{ profileDir?: unknown }>('app_info')
      .then((i) => setProfileDir(typeof i.profileDir === 'string' ? i.profileDir : ''))
      .catch(() => undefined);
  }, [shell, load]);

  const backUp = async () => {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const made = await ops.create();
      setNote(`Backed up to ${made.archiveDir}`);
      await load();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const askRestore = (b: BackupEntry) =>
    setConfirm({
      title: 'Restore this backup?',
      body: `This replaces all of your chats, settings and approvals with the ones in ${b.name}. Nothing changes until you restart Helm; then your current database is kept in a "before-restore" folder inside Helm's data folder. Sign-ins and keys are not part of a backup and are not touched. To go ahead, press "${RESTORE_CONFIRM}".`,
      confirm: RESTORE_CONFIRM,
      danger: true,
      onConfirm: async () => {
        setError(null);
        setNote(null);
        try {
          await ops.restore(b.path, RESTORE_CONFIRM);
          setNote('Restore staged. Restart Helm to apply it.');
          await load();
        } catch (e) {
          setError(errorText(e));
        }
      },
    });

  return (
    <div className="data-pane" data-testid="settings-data">
      <SettingRow testId="data-profile" title="Where your data is" description="Chats, settings, logs and attachments live in this folder on this computer.">
        <span className="data-path" data-testid="data-profile-path">
          {profileDir || 'Unknown'}
        </span>
      </SettingRow>
      <SettingRow
        testId="data-backup"
        title="Back up"
        description={`Writes a plain folder under ${list?.defaultDir ?? 'Documents'} that you can open and copy. It is a consistent copy even while Helm is running.`}
      >
        <Button variant="primary" data-testid="data-backup-button" disabled={busy} onClick={() => void backUp()}>
          Make a backup
        </Button>
      </SettingRow>
      {list?.pending && (
        <div className="data-pending" role="status" data-testid="data-restore-pending">
          A restore from {list.pending} is waiting. Restart Helm to apply it.
        </div>
      )}
      <SettingRow testId="data-backups" title="Backups found" description={list && list.backups.length === 0 ? 'None yet.' : undefined}>
        <span />
      </SettingRow>
      {list?.backups.map((b) => (
        <div className="data-row" key={b.path} data-testid="data-backup-row" data-path={b.path}>
          <div className="data-row-text">
            <div className="data-row-name">{new Date(b.createdAt).toLocaleString()}</div>
            <div className="data-row-meta">
              {size(b.dbBytes)} database, {b.entries} folders and files · {b.path}
            </div>
          </div>
          <Button variant="secondary" data-testid="data-restore-button" disabled={busy || !!list.pending} onClick={() => askRestore(b)}>
            Restore…
          </Button>
        </div>
      ))}
      {note && (
        <div className="setting-desc" role="status" data-testid="data-note">
          {note}
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="data-error">
          {error}
        </div>
      )}
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}
