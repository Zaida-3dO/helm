// The settings frame: a centred 620 px modal on Ctrl+, with wrapping tabs, a search
// field at the end of the tab row (the first Escape clears it), and the selected pane's body.
// Panes come from the registry; each pane decides whether it saves per control or with a Save.

import { useEffect, useState, useSyncExternalStore } from 'react';
import { useDialog, useShell } from '../shell/context.ts';
import { Dialog, useLayer } from '../ui/index.ts';
import { filterPanes, settingsPanes, subscribeSettingsPanes, peekRequestedPane, clearRequestedPane } from './registry.ts';

export const SETTINGS_DIALOG = 'settings';

export function SettingsDialog() {
  const { open, close } = useDialog(SETTINGS_DIALOG);
  if (!open) return null;
  return <SettingsBody close={close} />;
}

function SettingsBody({ close }: { close: () => void }) {
  const panes = useSyncExternalStore(subscribeSettingsPanes, settingsPanes);
  const [tab, setTab] = useState<string | null>(peekRequestedPane);
  useEffect(() => clearRequestedPane, []);
  const [query, setQuery] = useState('');
  useLayer('search', true, () => setQuery(''), () => query.length > 0);
  const searching = query.trim().length > 0;
  const shown = filterPanes(panes, query);
  const current = panes.find((p) => p.id === tab) ?? panes[0];
  const version = useAppVersion();
  const footer = (
    <>
      <span className="settings-version" data-testid="settings-version">
        Helm <span className="muted">{version ?? ''}</span>
      </span>
      <span className="settings-foot-note">Runs Claude Code under your own login</span>
    </>
  );
  return (
    <Dialog title="Settings" onClose={close} wide testId="settings" footer={footer}>
      <div className="settings-tabs" role="tablist">
        {panes.map((p) => (
          <button
            key={p.id}
            type="button"
            role="tab"
            className="settings-tab"
            aria-selected={!searching && p.id === current?.id}
            data-tab={p.id}
            onClick={() => {
              setQuery('');
              setTab(p.id);
            }}
          >
            {p.title}
          </button>
        ))}
        <input
          type="search"
          className="settings-search"
          placeholder="Search settings"
          aria-label="Search settings"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          data-autofocus=""
        />
      </div>
      <div className="settings-body">
        {panes.length === 0 && <p className="muted">No settings yet.</p>}
        {searching && shown.length === 0 && <p className="secondary">No settings match "{query.trim()}"</p>}
        {searching
          ? shown.map((p) => (
              <section key={p.id} data-testid="settings-pane" data-pane={p.id}>
                <h4 className="settings-pane-label">{p.title}</h4>
                {p.render()}
              </section>
            ))
          : current && (
              <section data-testid="settings-pane" data-pane={current.id}>
                {current.render()}
              </section>
            )}
      </div>
    </Dialog>
  );
}

/** The app's version from the host's `app_info`; null until it answers. */
function useAppVersion(): string | null {
  const shell = useShell();
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    shell
      .api<{ version?: unknown }>('app_info')
      .then((i) => live && setVersion(typeof i?.version === 'string' ? i.version : null))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [shell]);
  return version;
}
