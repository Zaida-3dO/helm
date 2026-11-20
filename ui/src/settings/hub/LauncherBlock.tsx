// Settings → Hub: the small always-on launcher. While it runs on this PC the hub can start Helm
// here when a message arrives, and bring it back after a crash. No hub paired, nothing to show
// (without a hub nobody can ask, and the host registers nothing), unless the person switched it
// on before unpairing: then it stays, so it can be switched off.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { hubOps, type LauncherState } from '../../api/hub.ts';
import { useShell } from '../../shell/context.ts';
import { Checkbox, SettingRow } from '../../ui/index.ts';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

/** The line under the switch: whether the process runs now, and whether the hub could wake
 * this PC through it. */
export function launcherStateLine(st: LauncherState | null): string {
  if (!st) return 'Loading…';
  return `${st.running ? 'Running' : 'Not running'} · Wake from the hub: ${st.wakeReady ? 'ready' : 'not set up'}`;
}

export function LauncherBlock({ paired }: { paired: boolean }) {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const [st, setSt] = useState<LauncherState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void ops
      .launcher()
      .then((s) => live && setSt(s))
      .catch((e: unknown) => live && setError(errorText(e)));
    return () => {
      live = false;
    };
  }, [ops, paired]);

  const set = useCallback(
    (enabled: boolean) => {
      setBusy(true);
      void ops
        .saveLauncher(enabled)
        .then((s) => {
          setSt(s);
          setError(null);
        })
        .catch((e: unknown) => setError(errorText(e)))
        .finally(() => setBusy(false));
    },
    [ops],
  );

  if (!paired && !(st?.enabled || st?.registered)) return null;
  return (
    <>
      <SettingRow
        testId="hub-launcher"
        title="Start Helm when a message arrives, and restart it if it crashes"
        description="A small always-on helper lets the hub start this PC's Helm when a message arrives, and bring it back within a minute if Helm stops by itself. It starts at logon and opens no window."
      >
        <Checkbox
          checked={st?.enabled ?? false}
          disabled={busy || st === null}
          aria-label="Start Helm when a message arrives, and restart it if it crashes"
          data-testid="hub-launcher-box"
          onChange={(on) => set(on)}
        />
      </SettingRow>
      <div className="hub-launcher-state muted" data-testid="hub-launcher-state">
        {launcherStateLine(st)}
      </div>
      {error && (
        <div className="ui-field-error" role="alert" data-testid="hub-launcher-error">
          {error}
        </div>
      )}
    </>
  );
}
