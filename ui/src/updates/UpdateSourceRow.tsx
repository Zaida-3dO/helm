// Settings → General: the folder Helm looks in for a newer setup file. Nothing is downloaded;
// point it at the folder where the installer build writes `Helm_<version>_x64-setup.exe`.

import { useEffect, useState } from 'react';
import { updateOps, type UpdateStatus } from '../api/updates.ts';
import { useShell } from '../shell/context.ts';
import { Button, SettingRow, toast } from '../ui/index.ts';
import './updates.css';

function describe(s: UpdateStatus | null): string {
  if (!s || !s.source) return 'Helm looks in this folder every 10 minutes for a setup file newer than this version. Nothing is downloaded.';
  if (s.error) return s.error;
  return s.available ? `Version ${s.available.version} is ready: restart from the top bar.` : `No setup file newer than ${s.current} in this folder.`;
}

export function UpdateSourceRow() {
  const shell = useShell();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    updateOps(shell.api)
      .status()
      .then(setStatus)
      .catch(() => undefined);
  }, [shell]);
  const apply = (run: Promise<UpdateStatus | null>) => {
    setBusy(true);
    run
      .then((s) => s && setStatus(s))
      .catch((e: unknown) => toast.show({ kind: 'error', text: String((e as Error).message ?? e) }))
      .finally(() => setBusy(false));
  };
  const ops = updateOps(shell.api);
  const choose = () => apply(ops.pickFolder().then(({ path }) => (path ? ops.setSource(path) : null)));
  return (
    <SettingRow title="Update folder" description={describe(status)} testId="setting-update-folder">
      {status?.source && (
        <span className="update-source-path" title={status.source} data-testid="update-folder-path">
          {status.source}
        </span>
      )}
      <Button variant="secondary" onClick={choose} disabled={busy} data-testid="update-folder-choose">
        {status?.source ? 'Change…' : 'Choose…'}
      </Button>
      {status?.source && (
        <>
          <Button variant="secondary" onClick={() => apply(ops.check())} disabled={busy} data-testid="update-folder-check">
            Check now
          </Button>
          <Button variant="secondary" onClick={() => apply(ops.setSource(null))} disabled={busy} data-testid="update-folder-clear">
            Clear
          </Button>
        </>
      )}
    </SettingRow>
  );
}
