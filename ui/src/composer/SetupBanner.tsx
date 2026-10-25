// The setup card above the box: shown while Node or Claude Code cannot be found, so a message
// could not run. The draft stays editable; Send is disabled until the runtime check passes. The
// check is the one Settings → Runtime shows.

import { useEffect, useState } from 'react';
import { useDialog, useShell } from '../shell/context.ts';
import { requestPane } from '../settings/registry.ts';
import { SETTINGS_DIALOG } from '../settings/SettingsDialog.tsx';
import type { RuntimeStatus } from '../settings/runtime/model.ts';
import { Button } from '../ui/controls.tsx';
import { IconChevronRight, IconWarning } from '../ui/icons.tsx';

/** What the runtime check found wrong (empty when ready or not yet known). Refreshes when Settings closes. */
export function useRuntimeProblems(): string[] {
  const shell = useShell();
  const settings = useDialog(SETTINGS_DIALOG);
  const [problems, setProblems] = useState<string[]>([]);
  useEffect(() => {
    if (settings.open) return;
    let live = true;
    shell
      .api<RuntimeStatus>('runtime_status', {})
      .then((s) => live && setProblems(s.problems ?? []))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [shell, settings.open]);
  return problems;
}

export function SetupBanner({ problems }: { problems: string[] }) {
  const shell = useShell();
  const [details, setDetails] = useState(false);
  if (problems.length === 0) return null;
  return (
    <div className="composer-setup" data-testid="composer-setup" role="status">
      <IconWarning size={16} />
      <div className="composer-setup-body">
        <div className="composer-setup-title">Connect Claude Code to continue this chat</div>
        <div className="composer-setup-sub">You can still read this chat and edit your draft.</div>
        <button type="button" className="composer-setup-toggle" aria-expanded={details} data-testid="composer-setup-details" onClick={() => setDetails((d) => !d)}>
          <IconChevronRight size={11} className={details ? 'open' : ''} /> Setup details
        </button>
        {details && (
          <ul className="composer-setup-list">
            {problems.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        )}
      </div>
      <Button
        variant="secondary"
        data-testid="composer-setup-open"
        onClick={() => {
          requestPane('runtime');
          shell.openDialog(SETTINGS_DIALOG);
        }}
      >
        Set up →
      </Button>
    </div>
  );
}
