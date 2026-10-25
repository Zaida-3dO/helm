// The settings frame: a centred modal of fixed size on Ctrl+,. The title, the pill tabs and the
// search field stay put (the first Escape clears the search); only the selected pane's body scrolls.
// A search lists every matching row from every pane under its pane's name (see search.ts).
// Panes come from the registry; each pane decides whether it saves per control or with a Save.

import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react';
import { useDialog, useShell } from '../shell/context.ts';
import { Dialog, useLayer } from '../ui/index.ts';
import { IconSearch } from '../ui/icons.tsx';
import { applySearch, clearHighlight } from './search.ts';
import { SettingsWatchers } from './watchers.tsx';
import { settingsPanes, subscribeSettingsPanes, peekRequestedPane, clearRequestedPane } from './registry.ts';

export const SETTINGS_DIALOG = 'settings';

export function SettingsDialog() {
  const { open, close } = useDialog(SETTINGS_DIALOG);
  return (
    <>
      <SettingsWatchers />
      {open && <SettingsBody close={close} />}
    </>
  );
}

function SettingsBody({ close }: { close: () => void }) {
  const panes = useSyncExternalStore(subscribeSettingsPanes, settingsPanes);
  const [tab, setTab] = useState<string | null>(peekRequestedPane);
  useEffect(() => clearRequestedPane, []);
  const [query, setQuery] = useState('');
  useLayer('search', true, () => setQuery(''), () => query.length > 0);
  // Searching mounts every pane (their rows are what gets searched), and each pane loads its own
  // data, so wait for a pause in typing before starting.
  const [applied, setApplied] = useState('');
  useEffect(() => {
    if (query.trim() === '') return setApplied('');
    const t = setTimeout(() => setApplied(query), 250);
    return () => clearTimeout(t);
  }, [query]);
  const searching = applied.trim().length > 0;
  const bodyRef = useRef<HTMLDivElement>(null);
  const [hits, setHits] = useState(0);
  const current = panes.find((p) => p.id === tab) ?? panes[0];
  const version = useAppVersion();
  // Filter the rendered rows after every render and whenever a pane changes its own content
  // (data arriving late), so a search never shows a stale list.
  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    if (!searching) {
      applySearch(body, '');
      clearHighlight();
      return;
    }
    const run = () => setHits(applySearch(body, applied));
    run();
    let frame = 0;
    const watch = new MutationObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(run);
    });
    watch.observe(body, { childList: true, subtree: true, characterData: true });
    return () => {
      watch.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [applied, searching, panes]);
  useEffect(() => clearHighlight, []);
  const footer = (
    <>
      <span className="settings-version" data-testid="settings-version">
        Helm <span className="muted">{version ?? ''}</span>
      </span>
      <span className="settings-foot-note">Runs Claude Code under your own login</span>
    </>
  );
  const tabs = (
    <div className="settings-tabs" role="tablist">
        {panes.map((p) => (
          <button
            key={p.id}
            type="button"
            role="tab"
            className="settings-tab"
            aria-selected={query.trim() === '' && p.id === current?.id}
            data-tab={p.id}
            onClick={() => {
              setQuery('');
              setApplied('');
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
  );
  return (
    <Dialog title="Settings" onClose={close} wide testId="settings" footer={footer} intro={tabs}>
      <div className="settings-body" ref={bodyRef}>
        {panes.length === 0 && <p className="muted">No settings yet.</p>}
        {searching && hits === 0 && (
          <div className="settings-nomatch" data-testid="settings-nomatch">
            <IconSearch size={22} />
            <span>No settings match "{applied.trim()}"</span>
          </div>
        )}
        {searching
          ? panes.map((p) => (
              <section key={p.id} data-testid="settings-pane" data-pane={p.id} data-title={p.title}>
                <h4 className="settings-pane-label">{p.title}</h4>
                {p.render()}
              </section>
            ))
          : current && (
              <section data-testid="settings-pane" data-pane={current.id} data-title={current.title}>
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
