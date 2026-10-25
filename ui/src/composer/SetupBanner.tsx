// The setup card above the box: shown while Node or Claude Code cannot be found, so a message
// could not run. The draft stays editable; Send is disabled until the runtime check passes. The
// check is the one Settings → Runtime shows.

import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { useDialog, useShell } from '../shell/context.ts';
import { requestPane } from '../settings/registry.ts';
import { SETTINGS_DIALOG } from '../settings/SettingsDialog.tsx';
import { runtimeCheck, type RuntimeCheck } from './runtimeCheck.ts';
import { Button } from '../ui/controls.tsx';
import { IconChevronRight, IconWarning } from '../ui/icons.tsx';

/**
 * What the runtime check found. One check serves the whole app; it is asked again when the window
 * comes back to the front while a problem stands, when Settings closes, and from the card's Retry.
 */
export function useRuntimeCheck(): RuntimeCheck & { retry: () => void } {
  const shell = useShell();
  const settings = useDialog(SETTINGS_DIALOG);
  const check = useSyncExternalStore(runtimeCheck.subscribe, runtimeCheck.get);
  const api = shell.api as (op: string, args: unknown) => Promise<unknown>;
  const wasOpen = useRef(false);
  useEffect(() => {
    runtimeCheck.ensure(api);
    const focus = () => runtimeCheck.get().blocked && void runtimeCheck.refresh(api);
    window.addEventListener('focus', focus);
    return () => window.removeEventListener('focus', focus);
  }, [api]);
  useEffect(() => {
    if (wasOpen.current && !settings.open) void runtimeCheck.refresh(api);
    wasOpen.current = settings.open;
  }, [settings.open, api]);
  return { ...check, retry: () => void runtimeCheck.refresh(api) };
}

export function SetupBanner({ check, onRetry }: { check: RuntimeCheck; onRetry: () => void }) {
  const shell = useShell();
  const [details, setDetails] = useState(false);
  if (!check.blocked) return null;
  const problems = check.problems;
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
      <Button variant="text" data-testid="composer-setup-retry" onClick={onRetry}>
        Check again
      </Button>
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
