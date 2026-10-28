// Settings → GitHub: the pull request view switch (off by default, only the user turns it on) and
// "Check sign-in". Helm reaches GitHub through the GitHub command-line tool under the user's own
// sign-in; while the switch is off nothing here or in the PR view contacts GitHub.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, SettingRow } from '../../ui/index.ts';
import { githubOps, splitError, type GitHubSettings, type Viewer } from './api.ts';
import './github.css';

function errorText(e: unknown): string {
  return splitError(String((e as { message?: unknown })?.message ?? e)).text;
}

export function GitHubPane() {
  const shell = useShell();
  const ops = useMemo(() => githubOps(shell.api), [shell.api]);
  const [settings, setSettings] = useState<GitHubSettings | null>(null);
  const [viewer, setViewer] = useState<Viewer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(
    () =>
      ops
        .settings()
        .then(setSettings)
        .catch((e: unknown) => setError(errorText(e))),
    [ops],
  );

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (on: boolean) => {
    if (!settings) return;
    setBusy(true);
    try {
      setSettings(await ops.save(on, settings.revision));
      setError(null);
      if (!on) setViewer(null);
    } catch (e) {
      // A stale revision: show the stored value before the message.
      await load();
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const check = async () => {
    setBusy(true);
    setCheckError(null);
    try {
      setViewer((await ops.viewer()).data);
    } catch (e) {
      setViewer(null);
      setCheckError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const on = settings?.enabled ?? false;
  return (
    <div className="github-pane" data-testid="settings-github">
      <SettingRow
        testId="github-pr-view"
        title="Pull requests in Helm"
        description="Open GitHub pull requests in the side panel: read the conversation, files and checks, and comment or review. Helm uses the GitHub command-line tool (gh) with your own sign-in and writes to GitHub only when you click. While this is off, Helm never contacts GitHub for it."
      >
        <Checkbox
          checked={on}
          disabled={busy || !settings}
          aria-label="Pull requests in Helm"
          data-testid="github-pr-view-box"
          onChange={(v) => void toggle(v)}
        />
      </SettingRow>
      <SettingRow
        testId="github-check"
        title="Check sign-in"
        description={on ? 'Asks GitHub who the GitHub command-line tool is signed in as.' : 'Turn on pull requests in Helm first.'}
      >
        {viewer && (
          <span className="github-viewer" data-testid="github-viewer">
            <span className="github-avatar" aria-hidden="true">
              {viewer.login.slice(0, 1).toUpperCase()}
            </span>
            Signed in as {viewer.name ? `${viewer.name} (${viewer.login})` : viewer.login}
          </span>
        )}
        <Button variant="secondary" data-testid="github-check-button" disabled={busy || !on} busy={busy && on} onClick={() => void check()}>
          Check sign-in
        </Button>
      </SettingRow>
      {checkError && (
        <div className="ui-field-error" role="alert" data-testid="github-check-error">
          {checkError}
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="github-error">
          {error}
        </div>
      )}
    </div>
  );
}
